use rosemary::{
    dataflow::{self, Flags, State},
    decode, emit, input,
    ir::{Operand, Program},
};

#[test]
fn packed_minmax_decode_registers_memory_and_preserve_scalar_state() {
    for (opcode, op) in [
        ("da", "pminub"),
        ("de", "pmaxub"),
        ("ea", "pminsw"),
        ("ee", "pmaxsw"),
    ] {
        for (encoding, operands) in [
            (
                format!("66 0f {opcode} c1"),
                vec![Operand::Vec(0), Operand::Vec(1)],
            ),
            (
                format!("66 45 0f {opcode} f7"),
                vec![Operand::Vec(14), Operand::Vec(15)],
            ),
        ] {
            let p = decode::decode(&input::hex(&encoding).unwrap(), 0, None).unwrap();
            assert_eq!(p.instructions[0].op, op);
            assert_eq!(p.instructions[0].operands, operands);
            assert_eq!(Program::from_text(&p.to_text().unwrap()).unwrap(), p);
        }
        let p = decode::decode(
            &input::hex(&format!("66 44 0f {opcode} 43 f0")).unwrap(),
            0,
            None,
        )
        .unwrap();
        assert!(
            emit::assembly(&p)
                .unwrap()
                .contains(&format!("{op} xmm8, xmmword ptr [rbx"))
        );
        let before = State {
            registers: [Some(42); 16],
            flags: Flags {
                cf: Some(true),
                zf: Some(false),
                of: Some(true),
                sf: Some(false),
                pf: Some(true),
            },
            direction: Some(true),
        };
        assert_eq!(dataflow::transfer(&p.instructions[0], before), before);
        for encoding in [
            format!("0f {opcode} c1"),
            format!("f3 0f {opcode} c1"),
            format!("f0 66 0f {opcode} c1"),
            format!("66 0f {opcode}"),
        ] {
            assert!(
                decode::decode(&input::hex(&encoding).unwrap(), 0, None).is_err(),
                "{encoding}"
            );
        }
        for operands in [
            "x:128:0 i:64:1",
            "r:64:0 x:128:1",
            "x:128:0 m:64:0:-:1:0",
            "m:128:0:-:1:0 x:128:1",
        ] {
            assert!(
                Program::from_text(&format!("rosemary-ir 1\nentry 0\n0 1 {op} {operands}\n"))
                    .is_err()
            );
        }
    }
    for op in ["pminuw", "pmaxsd", "pminsb", "pmaxuq"] {
        assert!(
            Program::from_text(&format!(
                "rosemary-ir 1\nentry 0\n0 1 {op} x:128:0 x:128:1\n"
            ))
            .is_err()
        );
    }
}
