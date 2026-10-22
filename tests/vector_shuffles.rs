use rosemary::{
    dataflow::{self, Flags, State},
    decode, input,
    ir::{Operand, Program},
};

#[test]
fn two_source_shuffles_decode_selectors_extensions_and_rip_relative_memory() {
    for (prefix, op) in [("", "shufps"), ("66", "shufpd")] {
        for select in 0..=255 {
            let p = decode::decode(
                &input::hex(&format!("{prefix} 45 0f c6 f7 {select:02x}")).unwrap(),
                0x1000,
                None,
            )
            .unwrap();
            let i = &p.instructions[0];
            assert_eq!(i.op, op);
            assert_eq!(
                i.operands,
                [Operand::Vec(14), Operand::Vec(15), Operand::Imm(select, 8)]
            );
            assert_eq!(Program::from_text(&p.to_text().unwrap()).unwrap(), p);
        }
        let bytes = input::hex(&format!("{prefix} 44 0f c6 05 10 00 00 00 ff")).unwrap();
        let p = decode::decode(&bytes, 0x1000, None).unwrap();
        assert_eq!(p.instructions[0].operands[0], Operand::Vec(8));
        assert!(
            matches!(p.instructions[0].operands[1], Operand::Mem {width:128,base:None,disp,..} if disp==0x1010+bytes.len() as u64)
        );
        let state = State {
            registers: [Some(42); 16],
            flags: Flags {
                zf: Some(true),
                sf: Some(false),
                cf: Some(true),
                of: Some(false),
                pf: Some(true),
            },
            direction: Some(true),
        };
        assert_eq!(dataflow::transfer(&p.instructions[0], state), state);
    }
    let p = decode::decode(&input::hex("66 0f c6 c0 01 0f c6 c0 e8").unwrap(), 0, None).unwrap();
    assert_eq!(p.instructions[0].op, "shufpd");
    assert_eq!(p.instructions[1].op, "shufps");
}

#[test]
fn two_source_shuffles_reject_wrong_forms_and_missing_selectors() {
    let bytes = input::hex("66 44 0f c6 84 24 ff ff ff ff ee").unwrap();
    for size in 0..bytes.len() {
        assert!(decode::decode(&bytes[..size], 0, None).is_err());
    }
    for encoding in ["f3 0f c6 c0 00", "f2 0f c6 c0 00", "f0 0f c6 c0 00"] {
        assert!(decode::decode(&input::hex(encoding).unwrap(), 0, None).is_err());
    }
    for op in ["shufps", "shufpd"] {
        for operands in [
            "x:128:0 x:128:1",
            "x:128:0 x:128:1 i:32:0",
            "x:128:0 m:64:0:-:1:0 i:8:0",
            "m:128:0:-:1:0 x:128:1 i:8:0",
            "x:128:0 x:128:1 i:8:100",
        ] {
            assert!(
                Program::from_text(&format!("rosemary-ir 1\nentry 0\n0 1 {op} {operands}\n"))
                    .is_err()
            );
        }
    }
}
