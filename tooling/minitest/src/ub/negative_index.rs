use crate::*;

#[test]
fn negative_index() {
    let locals = &[
        <[(); 2]>::get_ptype(),
        <()>::get_ptype(),
    ];

    let stmts = &[
        live(0),
        live(1),
        assign(
            local(1),
            load(index(local(0), const_int::<isize>(-1))),
        ),
    ];

    let p = small_program(locals, stmts);
    dump_program(&p);
    assert_ub(p, "out-of-bounds array access");
}
