use crate::*;

// Some Rust features are not supported, and are ignored by `minimize`.
// Those can be found by grepping "IGNORED".

/// A MIR statement becomes either a MiniRust statement or an intrinsic with some arguments, which
/// then starts a new basic block.
enum StatementResult {
    Statement(Statement),
    Intrinsic { intrinsic: IntrinsicOp, destination: PlaceExpr, arguments: List<ValueExpr> },
}

/// A MIR terminator becomes a MiniRust terminator, possibly preceded by a list
/// of statements to execute at the end of the previous basic block.
struct TerminatorResult {
    stmts: List<Statement>,
    terminator: Terminator,
}

impl<'cx, 'tcx> FnCtxt<'cx, 'tcx> {
    /// Translate the given basic block and insert it into `self` with the given name.
    /// May insert more than one block because some MIR statements turn into MiniRust terminators.
    pub fn translate_bb(&mut self, name: BbName, bb: &rs::BasicBlockData<'tcx>) {
        let mut cur_block_name = name;
        let mut cur_block_statements = List::new();
        for stmt in bb.statements.iter() {
            match self.translate_stmt(stmt) {
                StatementResult::Statement(stmt) => {
                    cur_block_statements.push(stmt);
                }
                StatementResult::Intrinsic { intrinsic, destination, arguments } => {
                    // Generate a fresh bb name.
                    let next_bb = self.fresh_bb_name();
                    // End the current block by jumping to the next one.
                    let terminator = Terminator::Intrinsic {
                        intrinsic,
                        arguments,
                        ret: destination,
                        next_block: Some(next_bb),
                    };
                    let cur_block = BasicBlock { statements: cur_block_statements, terminator };
                    let old = self.blocks.insert(cur_block_name, cur_block);
                    assert!(old.is_none()); // make sure we do not overwrite a bb
                    // Go on building the next block.
                    cur_block_name = next_bb;
                    cur_block_statements = List::new();
                }
            }
        }
        let TerminatorResult { stmts, terminator } = self.translate_terminator(bb.terminator());
        for stmt in stmts.iter() {
            cur_block_statements.push(stmt);
        }
        let cur_block = BasicBlock { statements: cur_block_statements, terminator };
        let old = self.blocks.insert(cur_block_name, cur_block);
        assert!(old.is_none()); // make sure we do not overwrite a bb
    }

    fn translate_stmt(&mut self, stmt: &rs::Statement<'tcx>) -> StatementResult {
        let span = stmt.source_info.span;
        StatementResult::Statement(match &stmt.kind {
            rs::StatementKind::Assign(box (place, rval)) => {
                let destination = self.translate_place(place, span);
                // Some things that MIR handles as rvalues are non-deterministic,
                // so MiniRust treats them differently.
                match rval {
                    rs::Rvalue::Cast(rs::CastKind::PointerExposeProvenance, operand, _) => {
                        let operand = self.translate_operand(operand, span);
                        return StatementResult::Intrinsic {
                            intrinsic: IntrinsicOp::PointerExposeProvenance,
                            destination,
                            arguments: list![operand],
                        };
                    }
                    rs::Rvalue::Cast(rs::CastKind::PointerWithExposedProvenance, operand, _) => {
                        // TODO untested so far! (Can't test because of `predict`)
                        let operand = self.translate_operand(operand, span);
                        return StatementResult::Intrinsic {
                            intrinsic: IntrinsicOp::PointerWithExposedProvenance,
                            destination,
                            arguments: list![operand],
                        };
                    }
                    _ => {}
                }
                let source = self.translate_rvalue(rval, span);
                Statement::Assign { destination, source }
            }
            rs::StatementKind::StorageLive(local) =>
                Statement::StorageLive(self.local_name_map[&local]),
            rs::StatementKind::StorageDead(local) =>
                Statement::StorageDead(self.local_name_map[&local]),
            rs::StatementKind::Retag(kind, place) => {
                let place = self.translate_place(place, span);
                let fn_entry = matches!(kind, rs::RetagKind::FnEntry);
                Statement::Validate { place, fn_entry }
            }
            rs::StatementKind::Deinit(place) => {
                let place = self.translate_place(place, span);
                Statement::Deinit { place }
            }
            rs::StatementKind::SetDiscriminant { place, variant_index } => {
                let place_ty =
                    rs::Place::ty_from(place.local, place.projection, &self.body, self.tcx).ty;
                let discriminant = self.discriminant_for_variant(place_ty, *variant_index, span);
                Statement::SetDiscriminant {
                    destination: self.translate_place(place, span),
                    value: discriminant,
                }
            }
            rs::StatementKind::Intrinsic(box rs::NonDivergingIntrinsic::Assume(op)) => {
                let op = self.translate_operand(op, span);
                // Doesn't return anything, get us a dummy place.
                let destination = build::zst_place();
                return StatementResult::Intrinsic {
                    intrinsic: IntrinsicOp::Assume,
                    destination,
                    arguments: list![op],
                };
            }
            rs::StatementKind::PlaceMention(place) => {
                let place = self.translate_place(place, span);
                Statement::PlaceMention(place)
            }
            x => rs::span_bug!(span, "statement not supported: {x:?}"),
        })
    }

    fn translate_terminator(&mut self, terminator: &rs::Terminator<'tcx>) -> TerminatorResult {
        let span = terminator.source_info.span;
        let terminator = match &terminator.kind {
            rs::TerminatorKind::Return => Terminator::Return,
            rs::TerminatorKind::Goto { target } => Terminator::Goto(self.bb_name_map[&target]),
            rs::TerminatorKind::Call { func, target, destination, args, .. } =>
                return self.translate_call(func, args, destination, target, span),
            rs::TerminatorKind::SwitchInt { discr, targets } => {
                let ty = discr.ty(&self.body, self.tcx);
                let ty = self.translate_ty(ty, span);

                let discr_op = self.translate_operand(discr, span);
                let (value, int_ty) = match ty {
                    Type::Bool => {
                        // If the value is a boolean we need to cast it to an integer first as MiniRust switch only operates on ints.
                        let Type::Int(u8_inttype) = <u8>::get_type() else { unreachable!() };
                        (
                            ValueExpr::UnOp {
                                operator: UnOp::Cast(CastOp::Transmute(Type::Int(u8_inttype))),
                                operand: GcCow::new(discr_op),
                            },
                            u8_inttype,
                        )
                    }
                    Type::Int(ity) => (discr_op, ity),
                    // FIXME: add support for switching on `char`
                    _ =>
                        rs::span_bug!(
                            span,
                            "SwitchInt terminator currently only supports int and bool."
                        ),
                };

                let cases = targets
                    .iter()
                    .map(|(value, target)| {
                        (int_from_bits(value, int_ty), self.bb_name_map[&target])
                    })
                    .collect();

                let fallback_block = targets.otherwise();
                let fallback = self.bb_name_map[&fallback_block];

                Terminator::Switch { value, cases, fallback }
            }
            rs::TerminatorKind::Unreachable => Terminator::Unreachable,
            rs::TerminatorKind::Assert { cond, expected, target, .. } => {
                let mut condition = self.translate_operand(cond, span);
                // Check equality of `condition` and `expected`.
                // We do this by inverting `condition` if `expected` is false
                // and then checking if `condition` is true.
                if !expected {
                    condition = build::not(condition);
                }

                // Create panic block in case of `expected != condition`
                let panic_bb = self.fresh_bb_name();
                let panic_block = BasicBlock { statements: list![], terminator: build::panic() };
                self.blocks.try_insert(panic_bb, panic_block).unwrap();

                let next_block = self.bb_name_map[target];
                Terminator::Switch {
                    value: build::bool_to_int::<u8>(condition),
                    cases: [(Int::from(1), next_block)].into_iter().collect(),
                    fallback: panic_bb,
                }
            }
            rs::TerminatorKind::Drop { place, target, .. } => {
                let ty = place.ty(&self.body, self.tcx).ty;
                let drop_in_place_fn = rs::Instance::resolve_drop_in_place(self.tcx, ty);
                let place = self.translate_place(place, span);
                let ptr_to_drop = build::addr_of(place, build::raw_void_ptr_ty());

                Terminator::Call {
                    callee: build::fn_ptr(self.cx.get_fn_name(drop_in_place_fn)),
                    calling_convention: CallingConvention::Rust,
                    arguments: list![ArgumentExpr::ByValue(ptr_to_drop)],
                    ret: zst_place(),
                    next_block: Some(self.bb_name_map[&target]),
                }
            }
            x => rs::span_bug!(span, "terminator not supported: {x:?}"),
        };

        TerminatorResult { terminator, stmts: List::new() }
    }

    fn translate_rs_intrinsic(
        &mut self,
        intrinsic: rs::Instance<'tcx>,
        args: &[rs::Spanned<rs::Operand<'tcx>>],
        destination: &rs::Place<'tcx>,
        target: &Option<rs::BasicBlock>,
        span: rs::Span,
    ) -> TerminatorResult {
        let param_env = rs::ParamEnv::reveal_all();
        let intrinsic_name = self.tcx.item_name(intrinsic.def_id());
        match intrinsic_name {
            rs::sym::assert_inhabited
            | rs::sym::assert_zero_valid
            | rs::sym::assert_mem_uninitialized_valid => {
                let ty = intrinsic.args.type_at(0);
                let requirement =
                    rs::layout::ValidityRequirement::from_intrinsic(intrinsic_name).unwrap();
                let should_panic =
                    !self.tcx.check_validity_requirement((requirement, param_env.and(ty))).unwrap();

                let terminator = if should_panic {
                    Terminator::Intrinsic {
                        intrinsic: IntrinsicOp::Panic,
                        arguments: list![],
                        ret: zst_place(),
                        next_block: None,
                    }
                } else {
                    Terminator::Goto(self.bb_name_map[&target.unwrap()])
                };
                return TerminatorResult { terminator, stmts: List::new() };
            }
            rs::sym::raw_eq =>
                return TerminatorResult {
                    stmts: List::new(),
                    terminator: Terminator::Intrinsic {
                        intrinsic: IntrinsicOp::RawEq,
                        arguments: args
                            .iter()
                            .map(|x| self.translate_operand(&x.node, x.span))
                            .collect(),
                        ret: self.translate_place(&destination, span),
                        next_block: target.as_ref().map(|t| self.bb_name_map[t]),
                    },
                },
            rs::sym::arith_offset => {
                let destination = self.translate_place(&destination, span);

                let val = ValueExpr::BinOp {
                    operator: BinOp::PtrOffset { inbounds: false },
                    left: GcCow::new(self.translate_operand(&args[0].node, span)),
                    right: GcCow::new(self.translate_operand(&args[1].node, span)),
                };

                let stmt = Statement::Assign { destination, source: val };
                let terminator = Terminator::Goto(self.bb_name_map[&target.unwrap()]);

                return TerminatorResult { stmts: list!(stmt), terminator };
            }
            rs::sym::size_of_val => {
                let destination = self.translate_place(&destination, span);

                let size = self.size_of(intrinsic.args.type_at(0));

                let stmt = Statement::Assign { destination, source: size };
                let terminator = Terminator::Goto(self.bb_name_map[&target.unwrap()]);

                return TerminatorResult { stmts: list!(stmt), terminator };
            }
            rs::sym::min_align_of_val => {
                let destination = self.translate_place(&destination, span);

                let align = self.align_of(intrinsic.args.type_at(0));

                let stmt = Statement::Assign { destination, source: align };
                let terminator = Terminator::Goto(self.bb_name_map[&target.unwrap()]);

                return TerminatorResult { stmts: list!(stmt), terminator };
            }
            rs::sym::unlikely | rs::sym::likely => {
                // FIXME: use the "fallback body" provided in the standard library.
                let destination = self.translate_place(&destination, span);
                let val = self.translate_operand(&args[0].node, span);

                let stmt = Statement::Assign { destination, source: val };
                let terminator = Terminator::Goto(self.bb_name_map[&target.unwrap()]);

                return TerminatorResult { stmts: list!(stmt), terminator };
            }
            name => rs::span_bug!(span, "unsupported Rust intrinsic `{}`", name),
        }
    }

    fn translate_call(
        &mut self,
        func: &rs::Operand<'tcx>,
        args: &[rs::Spanned<rs::Operand<'tcx>>],
        destination: &rs::Place<'tcx>,
        target: &Option<rs::BasicBlock>,
        span: rs::Span,
    ) -> TerminatorResult {
        // For now we only support calling specific functions, not function pointers.
        let rs::Operand::Constant(box f1) = func else { panic!() };
        let rs::mir::Const::Val(_, f2) = f1.const_ else { panic!() };
        let &rs::TyKind::FnDef(f, substs_ref) = f2.kind() else { panic!() };
        let param_env = rs::ParamEnv::reveal_all();
        let instance = rs::Instance::expect_resolve(self.tcx, param_env, f, substs_ref, span);

        if matches!(instance.def, rs::InstanceKind::Intrinsic(_)) {
            // A Rust intrinsic.
            return self.translate_rs_intrinsic(instance, args, destination, target, span);
        }

        let terminator = if self.tcx.crate_name(f.krate).as_str() == "intrinsics" {
            // Direct call to a MiniRust intrinsic.
            let intrinsic = match self.tcx.item_name(f).as_str() {
                "print" => IntrinsicOp::PrintStdout,
                "eprint" => IntrinsicOp::PrintStderr,
                "exit" => IntrinsicOp::Exit,
                "panic" => IntrinsicOp::Panic,
                "allocate" => IntrinsicOp::Allocate,
                "deallocate" => IntrinsicOp::Deallocate,
                "spawn" => IntrinsicOp::Spawn,
                "join" => IntrinsicOp::Join,
                "create_lock" => IntrinsicOp::Lock(IntrinsicLockOp::Create),
                "acquire" => IntrinsicOp::Lock(IntrinsicLockOp::Acquire),
                "release" => IntrinsicOp::Lock(IntrinsicLockOp::Release),
                "atomic_store" => IntrinsicOp::AtomicStore,
                "atomic_load" => IntrinsicOp::AtomicLoad,
                "compare_exchange" => IntrinsicOp::AtomicCompareExchange,
                "atomic_fetch_add" => IntrinsicOp::AtomicFetchAndOp(IntBinOp::Add),
                "atomic_fetch_sub" => IntrinsicOp::AtomicFetchAndOp(IntBinOp::Sub),
                name => panic!("unsupported MiniRust intrinsic `{}`", name),
            };
            Terminator::Intrinsic {
                intrinsic,
                arguments: args.iter().map(|x| self.translate_operand(&x.node, x.span)).collect(),
                ret: self.translate_place(&destination, span),
                next_block: target.as_ref().map(|t| self.bb_name_map[t]),
            }
        } else if is_panic_fn(&instance.to_string()) {
            // We can't translate this call, it takes a string. As a hack we just ignore the argument.
            Terminator::Intrinsic {
                intrinsic: IntrinsicOp::Panic,
                arguments: list![],
                ret: zst_place(),
                next_block: None,
            }
        } else {
            let abi = self
                .cx
                .tcx
                .fn_abi_of_instance(rs::ParamEnv::reveal_all().and((instance, rs::List::empty())))
                .unwrap();
            let conv = translate_calling_convention(abi.conv);

            let args: List<_> = args
                .iter()
                .map(|x| {
                    match &x.node {
                        rs::Operand::Move(place) =>
                            ArgumentExpr::InPlace(self.translate_place(place, x.span)),
                        op => ArgumentExpr::ByValue(self.translate_operand(op, x.span)),
                    }
                })
                .collect();

            Terminator::Call {
                callee: build::fn_ptr(self.cx.get_fn_name(instance)),
                calling_convention: conv,
                arguments: args,
                ret: self.translate_place(&destination, span),
                next_block: target.as_ref().map(|t| self.bb_name_map[t]),
            }
        };
        TerminatorResult { terminator, stmts: List::new() }
    }
}

// HACK to skip translating some functions we can't handle yet.
fn is_panic_fn(name: &str) -> bool {
    name == "core::panicking::panic" || name == "core::panicking::panic_nounwind"
}
