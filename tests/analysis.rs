use rosemary::{
    analysis::{self, EdgeKind, Resolution},
    decode::{RecursiveOptions, decode, decode_recursive},
    input,
    ir::{Instruction, Operand, Program},
};

fn recursive(hex: &str) -> rosemary::Result<Program> {
    decode_recursive(
        &input::hex(hex)?,
        0x1000,
        0x1000,
        RecursiveOptions::default(),
    )
}

#[test]
fn recursive_skips_data_and_follows_both_sides_of_branches() {
    let bytes = input::hex("eb 02 0f 05 75 01 c3 c3 ff").unwrap();
    assert!(decode(&bytes, 0x1000, None).is_err());
    let p = decode_recursive(&bytes, 0x1000, 0x1000, RecursiveOptions::default()).unwrap();
    assert_eq!(
        p.instructions.iter().map(|i| i.address).collect::<Vec<_>>(),
        [0x1000, 0x1004, 0x1006, 0x1007]
    );
    assert_eq!(Program::from_text(&p.to_text().unwrap()).unwrap(), p);
    let a = analysis::analyze(&p).unwrap();
    assert_eq!(a.blocks.len(), 4);
    assert!(a.blocks.iter().all(|b| b.reachable));
    assert_eq!(a.edges.len(), 5);
}

#[test]
fn recursive_call_policy_indirect_targets_and_limits() {
    let bytes = input::hex("e8 01 00 00 00 c3 0f 05").unwrap();
    assert!(
        decode_recursive(&bytes, 0x1000, 0x1000, RecursiveOptions::default())
            .unwrap_err()
            .contains("0x1006")
    );
    let p = decode_recursive(
        &bytes,
        0x1000,
        0x1000,
        RecursiveOptions {
            follow_calls: false,
            ..Default::default()
        },
    )
    .unwrap();
    assert_eq!(p.instructions.len(), 2);
    let p = recursive("e8 01 00 00 00 c3 c3").unwrap();
    let a = analysis::analyze(&p).unwrap();
    assert_eq!(a.function_entries, [0x1000, 0x1006]);
    assert_eq!(a.edges[0].kind, EdgeKind::Call);
    assert_eq!(a.edges[0].resolution, Resolution::Internal);
    assert_eq!(recursive("ff e0 0f 05").unwrap().instructions.len(), 1);
    assert_eq!(recursive("e9 ff ff ff 7f").unwrap().instructions.len(), 1);
    assert_eq!(recursive("eb fe").unwrap().instructions.len(), 1);
    assert!(recursive("90 0f 05").unwrap_err().contains("0x1001"));
    assert!(
        decode_recursive(
            &[0x90, 0xc3],
            0,
            0,
            RecursiveOptions {
                max_instructions: 1,
                ..Default::default()
            }
        )
        .unwrap_err()
        .contains("limit")
    );
    assert!(decode_recursive(&[0xc3], 0, 1, RecursiveOptions::default()).is_err());
    assert!(decode_recursive(&[0xc3], u64::MAX, u64::MAX, RecursiveOptions::default()).is_err());
}

#[test]
fn recursive_rejects_overlapping_paths_in_both_discovery_orders() {
    // Jump into the branch itself, into a future MOV, or backward into a MOV.
    for hex in ["eb ff", "75 01 b8 00 00 00 00 c3", "b8 00 00 00 00 eb fa"] {
        let error = recursive(hex).unwrap_err();
        assert!(
            error.contains("enters instruction") || error.contains("overlaps"),
            "{hex}: {error}"
        );
    }
}

#[test]
fn analysis_reports_loops_unreachable_blocks_and_uncertain_edges() {
    let p = Program::from_text("rosemary-ir 1\nentry 1000\n1000 2 jmp i:64:1005\n1002 3 nop\n1005 2 jne i:64:1005\n1007 2 call r:64:0\n1009 2 jmp i:64:1001\n100b 2 call i:64:9000\n100d 1 ret\n").unwrap();
    let a = analysis::analyze(&p).unwrap();
    assert_eq!(
        a.blocks.iter().map(|b| b.start).collect::<Vec<_>>(),
        [0x1000, 0x1002, 0x1005, 0x1007, 0x1009, 0x100b, 0x100d]
    );
    assert_eq!(a.blocks.iter().filter(|b| b.reachable).count(), 4);
    assert_eq!(a.loops, [vec![0x1005]]);
    assert!(
        a.edges
            .iter()
            .any(|e| e.kind == EdgeKind::Call && e.resolution == Resolution::Indirect)
    );
    assert!(
        a.edges
            .iter()
            .any(|e| e.target == Some(0x1001) && e.resolution == Resolution::Interior)
    );
    assert!(
        a.edges
            .iter()
            .any(|e| e.target == Some(0x9000) && e.resolution == Resolution::External)
    );
    assert!(a.text().contains("[unreachable]"));
    assert!(
        a.json()
            .contains("\"resolution\": \"instruction-interior\"")
    );
    assert!(a.dot().contains("b_1005 -> b_1005"));
    assert_eq!(a.json(), analysis::analyze(&p).unwrap().json());
}

#[test]
fn analysis_handles_large_cycles_without_recursive_host_stack() {
    let count = 20_000;
    let p = Program {
        entry: 0,
        instructions: (0..count)
            .map(|n| Instruction {
                address: n * 2,
                len: 2,
                op: "jmp".into(),
                operands: vec![Operand::Imm(((n + 1) % count) * 2, 64)],
            })
            .collect(),
    };
    let a = analysis::analyze(&p).unwrap();
    assert_eq!(a.blocks.len(), count as usize);
    assert_eq!(a.loops.len(), 1);
    assert_eq!(a.loops[0].len(), count as usize);
    assert!(a.blocks.iter().all(|b| b.reachable));
    assert!(a.text().contains("loop components: 1"));
}

#[test]
fn extended_instruction_encodings_and_invalid_forms() {
    let nops = input::hex("66 90 66 0f 1f 44 00 00 66 66 2e 0f 1f 84 00 00 00 00 00").unwrap();
    let p = decode(&nops, 0, None).unwrap();
    assert_eq!(
        p.instructions.iter().map(|i| i.len).collect::<Vec<_>>(),
        [2, 6, 11]
    );
    assert!(decode(&[0x66; 16], 0, None).is_err());
    let bytes = input::hex("f3 0f 1e fa 4d 63 c1 4d 0f af c1 4d 6b c1 fd 4d 69 c1 ff ff ff ff ff c0 48 ff c8 f7 d0 48 f7 d8 48 f7 c0 ff ff ff ff").unwrap();
    let p = decode(&bytes, 0, None).unwrap();
    assert_eq!(
        p.instructions
            .iter()
            .map(|i| i.op.as_str())
            .collect::<Vec<_>>(),
        [
            "endbr64", "movsxd", "imul", "imul", "imul", "inc", "dec", "not", "neg", "test"
        ]
    );
    assert_eq!(
        p.instructions[1].operands,
        [Operand::Reg(8, 64), Operand::Reg(9, 32)]
    );
    assert_eq!(
        p.instructions[3].operands[2],
        Operand::Imm((-3i64) as u64, 64)
    );
    assert_eq!(p.instructions[5].operands, [Operand::Reg(0, 32)]);
    assert_eq!(p.instructions[6].operands, [Operand::Reg(0, 64)]);
    for bytes in [
        "f3 0f 00",
        "f3 0f 1e fb",
        "0f af",
        "48 63",
        "f7 c8",
        "ff d8",
    ] {
        assert!(
            decode(&input::hex(bytes).unwrap(), 0, None).is_err(),
            "{bytes}"
        );
    }
    for line in [
        "imul m:64:0:-:1:0 r:64:1",
        "movsxd r:32:0 r:32:1",
        "movsxd r:64:0 i:32:1",
        "imul r:64:0 r:32:1",
        "neg i:64:1",
    ] {
        assert!(Program::from_text(&format!("rosemary-ir 1\nentry 0\n0 1 {line}\n")).is_err());
    }
}
