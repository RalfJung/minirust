use crate::*;

impl<'tcx> Ctxt<'tcx> {
    pub fn pointee_info_of(&self, ty: rs::Ty<'tcx>) -> PointeeInfo {
        let layout = self.rs_layout_of(ty);
        assert!(layout.is_sized(), "encountered unsized type: {ty}");
        let size = translate_size(layout.size());
        let align = translate_align(layout.align().abi);
        let inhabited = !layout.abi().is_uninhabited();
        let param_env = rs::ParamEnv::reveal_all();
        let freeze = ty.is_freeze(self.tcx, param_env);

        PointeeInfo { size, align, inhabited, freeze }
    }

    pub fn pointee_info_of_smir(&self, ty: smir::Ty) -> PointeeInfo {
        self.pointee_info_of(smir::internal(self.tcx, ty))
    }

    pub fn translate_ty_smir(&mut self, ty: smir::Ty, span: rs::Span) -> Type {
        self.translate_ty(smir::internal(self.tcx, ty), span)
    }

    pub fn translate_ty(&mut self, ty: rs::Ty<'tcx>, span: rs::Span) -> Type {
        if let Some(mini_ty) = self.ty_cache.get(&ty) {
            return *mini_ty;
        }

        let mini_ty = match ty.kind() {
            rs::TyKind::Bool => Type::Bool,
            rs::TyKind::Int(t) => {
                let sz = rs::abi::Integer::from_int_ty(&self.tcx, *t).size();
                Type::Int(IntType { size: translate_size(sz), signed: Signedness::Signed })
            }
            rs::TyKind::Uint(t) => {
                let sz = rs::abi::Integer::from_uint_ty(&self.tcx, *t).size();
                Type::Int(IntType { size: translate_size(sz), signed: Signedness::Unsigned })
            }
            rs::TyKind::Tuple(ts) => {
                let layout = self.rs_layout_of(ty);
                let size = translate_size(layout.size());
                let align = translate_align(layout.align().abi);

                let fields = ts
                    .iter()
                    .enumerate()
                    .map(|(i, t)| {
                        let t = self.translate_ty(t, span);
                        let offset = layout.fields().offset(i);
                        let offset = translate_size(offset);

                        (offset, t)
                    })
                    .collect();

                Type::Tuple { fields, size, align }
            }
            rs::TyKind::Adt(adt_def, _) if adt_def.is_box() => {
                let ty = ty.boxed_ty();
                let pointee = self.pointee_info_of(ty);
                Type::Ptr(PtrType::Box { pointee })
            }
            rs::TyKind::Adt(adt_def, sref) if adt_def.is_struct() => {
                let (fields, size, align) = self.translate_non_enum_adt(ty, *adt_def, sref, span);
                Type::Tuple { fields, size, align }
            }
            rs::TyKind::Adt(adt_def, sref) if adt_def.is_union() => {
                let (fields, size, align) = self.translate_non_enum_adt(ty, *adt_def, sref, span);
                let chunks = calc_chunks(fields, size);
                Type::Union { fields, size, align, chunks }
            }
            rs::TyKind::Adt(adt_def, sref) if adt_def.is_enum() =>
                self.translate_enum(ty, *adt_def, sref, span),
            rs::TyKind::Ref(_, ty, mutbl) => {
                let pointee = self.pointee_info_of(*ty);
                let mutbl = translate_mutbl(*mutbl);
                Type::Ptr(PtrType::Ref { pointee, mutbl })
            }
            rs::TyKind::RawPtr(ty, _mutbl) => {
                let _pointee = self.pointee_info_of(*ty); // just to make sure that we can translate this type
                Type::Ptr(PtrType::Raw)
            }
            rs::TyKind::Array(ty, c) => {
                let count = Int::from(c.eval_target_usize(self.tcx, rs::ParamEnv::reveal_all()));
                let elem = GcCow::new(self.translate_ty(*ty, span));
                Type::Array { elem, count }
            }
            rs::TyKind::FnPtr(_sig) => Type::Ptr(PtrType::FnPtr),
            rs::TyKind::Never =>
                build::enum_ty::<u8>(&[], Discriminator::Invalid, build::size(0), build::align(1)),
            x => rs::span_bug!(span, "TyKind not supported: {x:?}"),
        };
        self.ty_cache.insert(ty, mini_ty);
        mini_ty
    }

    /// Constructs the fields of a given variant.
    pub fn translate_adt_variant_fields(
        &mut self,
        shape: &rs::FieldsShape<rs::FieldIdx>,
        variant: &rs::VariantDef,
        sref: rs::GenericArgsRef<'tcx>,
        span: rs::Span,
    ) -> Fields {
        variant
            .fields
            .iter_enumerated()
            .map(|(i, field)| {
                let ty = field.ty(self.tcx, sref);
                // Field types can be non-normalized even if the ADT type was normalized
                // (due to associated types on the fields).
                let ty = self.tcx.normalize_erasing_regions(rs::ParamEnv::reveal_all(), ty);
                let ty = self.translate_ty(ty, span);
                let offset = shape.offset(i.into());
                let offset = translate_size(offset);

                (offset, ty)
            })
            .collect()
    }

    fn translate_non_enum_adt(
        &mut self,
        ty: rs::Ty<'tcx>,
        adt_def: rs::AdtDef<'tcx>,
        sref: rs::GenericArgsRef<'tcx>,
        span: rs::Span,
    ) -> (Fields, Size, Align) {
        let layout = self.rs_layout_of(ty);
        let fields = self.translate_adt_variant_fields(
            layout.fields(),
            adt_def.non_enum_variant(),
            sref,
            span,
        );
        let size = translate_size(layout.size());
        let align = translate_align(layout.align().abi);

        (fields, size, align)
    }
}

pub fn translate_mutbl(mutbl: rs::Mutability) -> Mutability {
    match mutbl {
        rs::Mutability::Mut => Mutability::Mutable,
        rs::Mutability::Not => Mutability::Immutable,
    }
}

pub fn translate_mutbl_smir(mutbl: smir::Mutability) -> Mutability {
    match mutbl {
        smir::Mutability::Mut => Mutability::Mutable,
        smir::Mutability::Not => Mutability::Immutable,
    }
}

pub fn translate_size(size: rs::Size) -> Size {
    Size::from_bytes_const(size.bytes())
}

pub fn translate_align(align: rs::Align) -> Align {
    Align::from_bytes(align.bytes()).unwrap()
}

pub fn translate_calling_convention(conv: rs::Conv) -> CallingConvention {
    match conv {
        rs::Conv::C => CallingConvention::C,
        rs::Conv::Rust => CallingConvention::Rust,
        _ => todo!(),
    }
}
