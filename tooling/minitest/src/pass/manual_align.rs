use crate::*;

#[test]
fn manual_align() {
    let locals = &[
        <[u8; 64]>::get_ptype(),
        <usize>::get_ptype()
    ];

    let stmts = &[
        live(0),
        live(1),
        assign( // _1 = (&raw _0) as usize;
            local(1),
            ptr_to_int(
                addr_of(local(0), <*const u8>::get_type()),
            ),
        ),
        assign( // _1 = (8 + (_1 / 8 * 8)) - _1; This guarantees alignment of 8 for (&raw _0) + _1
            local(1),
            sub::<usize>(
                add::<usize>(
                    const_int::<usize>(8),
                    mul::<usize>(
                        div::<usize>(
                            load(local(1)),
                            const_int::<usize>(8)
                        ),
                        const_int::<usize>(8)
                    ),
                ),
                load(local(1))
            )
        ),
        assign(
            deref(
                ptr_offset(
                    addr_of(local(0), <*mut u64>::get_type()),
                    load(local(1)),
                    InBounds::Yes
                ),
                <u64>::get_ptype()
            ),
            const_int::<u64>(42)
        ),
    ];

    let p = small_program(locals, stmts);
    dump_program(&p);
    assert_stop(p);
}
