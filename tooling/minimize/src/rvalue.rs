use crate::*;

impl<'cx, 'tcx> FnCtxt<'cx, 'tcx> {
    pub fn translate_rvalue(&mut self, rv: &rs::Rvalue<'tcx>, span: rs::Span) -> ValueExpr {
        self.translate_rvalue_smir(&smir::stable(rv), span)
    }

    pub fn translate_rvalue_smir(&mut self, rv: &smir::Rvalue, span: rs::Span) -> ValueExpr {
        match rv {
            smir::Rvalue::Use(operand) => self.translate_operand_smir(operand, span),
            smir::Rvalue::BinaryOp(bin_op, l, r) => {
                let lty_smir = l.ty(&self.locals_smir).unwrap();
                let lty = self.translate_ty_smir(lty_smir, span);

                let l = self.translate_operand_smir(l, span);
                let r = self.translate_operand_smir(r, span);

                use smir::BinOp::*;
                match (bin_op, lty) {
                    (Offset, Type::Ptr(_)) => build::ptr_offset(l, r, build::InBounds::Yes),

                    (Add, Type::Int(_)) => build::add(l, r),
                    (Sub, Type::Int(_)) => build::sub(l, r),
                    (Mul, Type::Int(_)) => build::mul(l, r),
                    (Div, Type::Int(_)) => build::div(l, r),
                    (Rem, Type::Int(_)) => build::rem(l, r),
                    (Shl, Type::Int(_)) => build::shl(l, r),
                    (Shr, Type::Int(_)) => build::shr(l, r),
                    (BitAnd, Type::Int(_)) => build::bit_and(l, r),
                    (BitOr, Type::Int(_)) => build::bit_or(l, r),
                    (BitXor, Type::Int(_)) => build::bit_xor(l, r),
                    (AddUnchecked, Type::Int(_)) => build::add_unchecked(l, r),
                    (SubUnchecked, Type::Int(_)) => build::sub_unchecked(l, r),
                    (MulUnchecked, Type::Int(_)) => build::mul_unchecked(l, r),
                    (ShlUnchecked, Type::Int(_)) => build::shl_unchecked(l, r),
                    (ShrUnchecked, Type::Int(_)) => build::shr_unchecked(l, r),

                    (Lt, _) => build::lt(l, r),
                    (Le, _) => build::le(l, r),
                    (Gt, _) => build::gt(l, r),
                    (Ge, _) => build::ge(l, r),
                    (Eq, _) => build::eq(l, r),
                    (Ne, _) => build::ne(l, r),

                    (Cmp, _) => {
                        let res = build::cmp(l, r);
                        // MiniRust expects an i8 for BinOp::Cmp but MIR uses an Ordering enum,
                        // so we have to transmute the result.
                        let ordering_ty: rs::Ty = self.tcx.ty_ordering_enum(None);
                        let ordering_ty: Type = self.translate_ty(ordering_ty, span);
                        build::transmute(res, ordering_ty)
                    }

                    (BitAnd, Type::Bool) => build::bool_and(l, r),
                    (BitOr, Type::Bool) => build::bool_or(l, r),
                    (BitXor, Type::Bool) => build::bool_xor(l, r),

                    (op, _) =>
                        rs::span_bug!(span, "Binary Op {op:?} not supported for type {lty_smir}."),
                }
            }
            smir::Rvalue::CheckedBinaryOp(op, l, r) => {
                let l = GcCow::new(self.translate_operand_smir(l, span));
                let r = GcCow::new(self.translate_operand_smir(r, span));

                let op = match op {
                    smir::BinOp::Add => BinOp::IntWithOverflow(IntBinOpWithOverflow::Add),
                    smir::BinOp::Sub => BinOp::IntWithOverflow(IntBinOpWithOverflow::Sub),
                    smir::BinOp::Mul => BinOp::IntWithOverflow(IntBinOpWithOverflow::Mul),
                    x => panic!("CheckedBinaryOp {x:?} not supported."),
                };
                ValueExpr::BinOp { operator: op, left: l, right: r }
            }
            smir::Rvalue::UnaryOp(unop, operand) => {
                let ty_smir = operand.ty(&self.locals_smir).unwrap();
                let ty = self.translate_ty_smir(ty_smir, span);
                let operand = self.translate_operand_smir(operand, span);

                use smir::UnOp::*;
                match (unop, ty) {
                    (Neg, Type::Int(_)) => build::neg(operand),
                    (Not, Type::Int(_)) => build::bit_not(operand),
                    (Not, Type::Bool) => build::not(operand),
                    (op, _) =>
                        rs::span_bug!(span, "UnOp {op:?} called with unsupported type {ty_smir}."),
                }
            }
            smir::Rvalue::Ref(_, bkind, place) => {
                let ty = place.ty(&self.locals_smir).unwrap();
                let pointee = self.pointee_info_of_smir(ty);

                let place = self.translate_place_smir(place, span);
                let target = GcCow::new(place);
                let mutbl = translate_mutbl_smir(bkind.to_mutable_lossy());

                let ptr_ty = PtrType::Ref { mutbl, pointee };

                ValueExpr::AddrOf { target, ptr_ty }
            }
            smir::Rvalue::AddressOf(_mutbl, place) => {
                let place = self.translate_place_smir(place, span);
                let target = GcCow::new(place);

                let ptr_ty = PtrType::Raw;

                ValueExpr::AddrOf { target, ptr_ty }
            }
            smir::Rvalue::Aggregate(agg, operands) => {
                let ty = rv.ty(&self.locals_smir).unwrap();
                let ty = self.translate_ty_smir(ty, span);
                match ty {
                    Type::Union { .. } => {
                        let smir::AggregateKind::Adt(_, _, _, _, Some(field_idx)) = agg else {
                            panic!()
                        };
                        assert_eq!(operands.len(), 1);
                        let expr = self.translate_operand_smir(&operands[0], span);
                        ValueExpr::Union {
                            field: (*field_idx).into(),
                            expr: GcCow::new(expr),
                            union_ty: ty,
                        }
                    }
                    Type::Tuple { .. } | Type::Array { .. } => {
                        let ops: List<_> =
                            operands.iter().map(|x| self.translate_operand_smir(x, span)).collect();
                        ValueExpr::Tuple(ops, ty)
                    }
                    Type::Enum { variants, .. } => {
                        let smir::AggregateKind::Adt(_, variant_idx, _, _, _) = agg else {
                            panic!()
                        };
                        let variant_ty = rv.ty(&self.locals_smir).unwrap();
                        let discriminant =
                            self.discriminant_for_variant_smir(variant_ty, *variant_idx, span);
                        let ops: List<_> =
                            operands.iter().map(|x| self.translate_operand_smir(x, span)).collect();

                        // We represent the multiple fields of an enum variant as a MiniRust tuple.
                        let data = GcCow::new(ValueExpr::Tuple(
                            ops,
                            variants.get(discriminant).unwrap().ty,
                        ));
                        ValueExpr::Variant { discriminant, data, enum_ty: ty }
                    }
                    x => rs::span_bug!(span, "Invalid aggregate type: {x:?}"),
                }
            }
            smir::Rvalue::CopyForDeref(place) =>
                ValueExpr::Load { source: GcCow::new(self.translate_place_smir(place, span)) },
            smir::Rvalue::Len(place) => {
                // as slices are unsupported as of now, we only need to care for arrays.
                let ty = place.ty(&self.locals_smir).unwrap();
                let Type::Array { elem: _, count } = self.translate_ty_smir(ty, span) else {
                    panic!()
                };
                ValueExpr::Constant(Constant::Int(count), <usize>::get_type())
            }
            smir::Rvalue::Discriminant(place) =>
                ValueExpr::GetDiscriminant {
                    place: GcCow::new(self.translate_place_smir(place, span)),
                },
            smir::Rvalue::Repeat(op, c) => {
                let c = c.eval_target_usize().unwrap();
                let c = Int::from(c);

                let elem_ty = op.ty(&self.locals_smir).unwrap();
                let elem_ty = self.translate_ty_smir(elem_ty, span);
                let op = self.translate_operand_smir(op, span);

                let ty = Type::Array { elem: GcCow::new(elem_ty), count: c };

                let ls = list![op; c];
                ValueExpr::Tuple(ls, ty)
            }
            smir::Rvalue::Cast(smir::CastKind::IntToInt, operand, ty) => {
                let operand_ty = operand.ty(&self.locals_smir).unwrap();
                let operand_ty = self.translate_ty_smir(operand_ty, span);
                let operand = self.translate_operand_smir(operand, span);
                let Type::Int(int_ty) = self.translate_ty_smir(*ty, span) else {
                    rs::span_bug!(span, "Attempting to IntToInt-Cast to non-int type!");
                };

                let operand = match operand_ty {
                    Type::Int(_) => operand,
                    // bool2int casts first go to u8, and then to the final type.
                    Type::Bool => build::transmute(operand, u8::get_type()),
                    _ =>
                        rs::span_bug!(
                            span,
                            "Attempting to cast non-int and non-boolean type to int!"
                        ),
                };
                ValueExpr::UnOp {
                    operator: UnOp::Cast(CastOp::IntToInt(int_ty)),
                    operand: GcCow::new(operand),
                }
            }
            smir::Rvalue::Cast(smir::CastKind::PointerExposeAddress, ..) => {
                unreachable!(
                    "PointerExposeAddress should have been handled on the statement level"
                );
            }
            smir::Rvalue::Cast(smir::CastKind::PointerWithExposedProvenance, ..) => {
                unreachable!(
                    "PointerWithExposedProvenance should have been handled on the statement level"
                );
            }
            smir::Rvalue::Cast(
                smir::CastKind::Transmute | smir::CastKind::PtrToPtr | smir::CastKind::FnPtrToPtr,
                operand,
                ty,
            ) => {
                let operand = self.translate_operand_smir(operand, span);
                let ty = self.translate_ty_smir(*ty, span);
                build::transmute(operand, ty)
            }
            smir::Rvalue::Cast(
                smir::CastKind::PointerCoercion(smir::PointerCoercion::ReifyFnPointer),
                func,
                _,
            ) => {
                let smir::Operand::Constant(f1) = func else { panic!() };
                let smir::TyKind::RigidTy(smir::RigidTy::FnDef(f, substs_ref)) = f1.ty().kind()
                else {
                    panic!()
                };
                let instance = smir::Instance::resolve(f, &substs_ref).unwrap();

                build::fn_ptr_internal(self.cx.get_fn_name_smir(instance).0.get_internal())
            }
            smir::Rvalue::NullaryOp(smir::NullOp::UbChecks, _ty) => {
                // Like Miri, since we are able to detect language UB ourselves we can disable these checks.
                // TODO: reflect the current session's ub_checks flag instead, once we are on a new enough rustc.
                build::const_bool(false)
            }
            x => rs::span_bug!(span, "rvalue failed to translate: {x:?}"),
        }
    }

    pub fn translate_operand(&mut self, operand: &rs::Operand<'tcx>, span: rs::Span) -> ValueExpr {
        self.translate_operand_smir(&smir::stable(operand), span)
    }

    pub fn translate_operand_smir(&mut self, operand: &smir::Operand, span: rs::Span) -> ValueExpr {
        match operand {
            smir::Operand::Constant(c) => self.translate_const_smir(&c.literal, span),
            smir::Operand::Copy(place) =>
                ValueExpr::Load { source: GcCow::new(self.translate_place_smir(place, span)) },
            smir::Operand::Move(place) =>
                ValueExpr::Load { source: GcCow::new(self.translate_place_smir(place, span)) },
        }
    }

    pub fn translate_place(&mut self, place: &rs::Place<'tcx>, span: rs::Span) -> PlaceExpr {
        self.translate_place_smir(&smir::stable(place), span)
    }

    pub fn translate_place_smir(&mut self, place: &smir::Place, span: rs::Span) -> PlaceExpr {
        // Initial state: start with the local the place is based on
        let expr = PlaceExpr::Local(self.local_name_map[&place.local.into()]);
        let place_ty = self.locals_smir[place.local].ty;
        // Fold over all projections
        let (expr, _place_ty) =
            place.projection.iter().fold((expr, place_ty), |(expr, place_ty), proj| {
                let this_ty = proj.ty(place_ty).unwrap();
                let this_expr = match proj {
                    smir::ProjectionElem::Field(f, _ty) => {
                        let indirected = GcCow::new(expr);
                        PlaceExpr::Field { root: indirected, field: (*f).into() }
                    }
                    smir::ProjectionElem::Deref => {
                        let x = GcCow::new(expr);
                        let x = ValueExpr::Load { source: x };
                        let x = GcCow::new(x);

                        let ty = self.translate_ty_smir(this_ty, span);

                        PlaceExpr::Deref { operand: x, ty }
                    }
                    smir::ProjectionElem::Index(loc) => {
                        let i = PlaceExpr::Local(self.local_name_map[&(*loc).into()]);
                        let i = GcCow::new(i);
                        let i = ValueExpr::Load { source: i };
                        let i = GcCow::new(i);
                        let root = GcCow::new(expr);
                        PlaceExpr::Index { root, index: i }
                    }
                    smir::ProjectionElem::Downcast(variant_idx) => {
                        let root = GcCow::new(expr);
                        let discriminant =
                            self.discriminant_for_variant_smir(this_ty, *variant_idx, span);
                        PlaceExpr::Downcast { root, discriminant }
                    }
                    x => rs::span_bug!(span, "Place Projection not supported: {:?}", x),
                };
                (this_expr, this_ty)
            });
        expr
    }
}
