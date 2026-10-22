use rosemary::{
    analysis::{self, EdgeKind},
    dataflow::{self, Flags, State},
    decode::{self, RecursiveOptions},
    emit, input,
    ir::{Instruction, Operand, Program},
};
#[cfg(target_arch = "x86_64")]
use std::{fmt::Write, fs, process::Command};

#[test]
fn counter_branches_roundtrip_recover_both_paths_and_form_cfg_loops() {
    for (opcode, op) in [
        (0xe0, "loopne"),
        (0xe1, "loope"),
        (0xe2, "loop"),
        (0xe3, "jrcxz"),
    ] {
        let p = decode::decode(&[opcode, 0xfe, 0xc3], 0x1000, None).unwrap();
        assert_eq!(p.instructions[0].op, op);
        assert_eq!(p.instructions[0].operands, [Operand::Imm(0x1000, 64)]);
        assert!(p.instructions[0].is_conditional());
        assert_eq!(Program::from_text(&p.to_text().unwrap()).unwrap(), p);
        let a = analysis::analyze(&p).unwrap();
        assert_eq!(a.loops, [vec![0x1000]]);
        assert!(
            a.edges
                .iter()
                .any(|e| e.kind == EdgeKind::Taken && e.target == Some(0x1000))
        );
        let assembly = emit::assembly(&p).unwrap();
        assert!(assembly.contains(&format!("{op} .L_1000_taken")));
        assert!(assembly.contains("jmp .L_1000\n"));
    }
    let p = decode::decode_recursive(
        &input::hex("e3 02 90 c3 c3").unwrap(),
        0x1000,
        0x1000,
        RecursiveOptions::default(),
    )
    .unwrap();
    assert_eq!(
        p.instructions.iter().map(|i| i.address).collect::<Vec<_>>(),
        [0x1000, 0x1002, 0x1003, 0x1004]
    );
    for bytes in [&[0xe2][..], &[0xe3], &[0x67, 0xe3, 0], &[0xf0, 0xe2, 0]] {
        assert!(decode::decode(bytes, 0, None).is_err());
    }
    assert!(decode::decode_recursive(&[0xe2, 0xff], 0, 0, RecursiveOptions::default()).is_err());
    assert!(Program::from_text("rosemary-ir 1\nentry 0\n0 2 loop r:64:0\n").is_err());
}

#[test]
fn counter_value_analysis_preserves_decrements_and_only_simplifies_pure_branches() {
    for op in ["loop", "loope", "loopne", "jrcxz"] {
        for count in [0, 1, 2, u64::MAX] {
            for zf in [false, true] {
                let p = Program::from_text(&format!("rosemary-ir 1\nentry 0\n0 1 mov r:64:1 i:64:{count:x}\n1 2 {op} i:64:4\n3 1 ret\n4 1 ret\n")).unwrap();
                let i = &p.instructions[1];
                let before = State {
                    registers: [Some(count); 16],
                    flags: Flags {
                        zf: Some(zf),
                        cf: Some(true),
                        of: Some(false),
                        sf: Some(true),
                        pf: Some(false),
                    },
                    direction: Some(true),
                };
                let after = dataflow::transfer(i, before);
                let mut expected = before;
                if op != "jrcxz" {
                    expected.registers[1] = Some(count.wrapping_sub(1));
                }
                assert_eq!(after, expected);
                let a = dataflow::analyze(&p).unwrap();
                let simplified = a.simplify().unwrap();
                if op == "jrcxz" {
                    assert_eq!(a.branch(1), Some(count == 0));
                    assert_eq!(
                        simplified.instructions[1].op,
                        if count == 0 { "jmp" } else { "nop" }
                    );
                } else {
                    assert_eq!(simplified.instructions[1], *i);
                    if count == 1 {
                        assert_eq!(a.branch(1), Some(false));
                    }
                }
            }
        }
    }
    let p = Program::from_text(
        "rosemary-ir 1\nentry 0\n0 1 mov r:64:1 i:64:0\n1 2 loop i:64:4\n3 1 ret\n4 1 ret\n",
    )
    .unwrap();
    let a = dataflow::analyze(&p).unwrap();
    assert_eq!(a.branch(1), Some(true));
    assert_eq!(a.states[3].unwrap().registers[1], Some(u64::MAX));
    assert!(a.states[2].is_none());
    assert!(
        a.annotation(1)
            .contains("rcx after decrement = 0xffffffffffffffff")
    );
}

#[test]
fn counter_loops_retract_early_facts_and_combine_partial_zf_information() {
    let p = Program::from_text(
        "rosemary-ir 1\nentry 0\n0 1 mov r:64:1 i:64:2\n1 2 loop i:64:1\n3 1 ret\n",
    )
    .unwrap();
    let a = dataflow::analyze(&p).unwrap();
    assert_eq!(a.branch(1), None);
    assert!(a.states[2].is_some());
    assert!(a.iterations < 100);
    for op in ["loope", "loopne"] {
        let zf_setup = if op == "loope" {
            "cmp r:64:0 i:64:1"
        } else {
            "cmp r:64:0 r:64:0"
        };
        let p = Program::from_text(&format!("rosemary-ir 1\nentry 0\n0 1 xor r:32:0 r:32:0\n1 1 {zf_setup}\n2 2 {op} i:64:5\n4 1 ret\n5 1 ret\n")).unwrap();
        let a = analysis::analyze(&p).unwrap();
        assert_eq!(a.values.branch(2), Some(false));
        assert!(a.edges.iter().any(|e| a.edge_feasible(e) == Some(false)));
        assert_eq!(a.values.simplify().unwrap().instructions[2].op, op);
    }
}

#[test]
#[cfg(target_arch = "x86_64")]
fn counter_branch_c_matches_native_registers_flags_and_branch_outcomes() {
    let p = Program {
        entry: 0,
        instructions: ["loop", "loope", "loopne", "jrcxz"]
            .iter()
            .enumerate()
            .map(|(n, op)| Instruction {
                address: n as u64,
                len: 1,
                op: (*op).into(),
                operands: vec![Operand::Imm(0x1000, 64)],
            })
            .collect(),
    };
    let mut c = emit::c(&p).unwrap();
    c.push_str(r#"
#include <assert.h>
int main(void) {
 const uint64_t counts[]={0,1,2,3,0xffffffff,UINT64_C(0x100000000),UINT64_MAX};
 for(unsigned j=0;j<sizeof(counts)/sizeof(counts[0]);j++)for(unsigned f=0;f<32;f++) {
  uint64_t initial=2|(f&1)|((uint64_t)((f>>1)&1)<<2)|((uint64_t)((f>>2)&1)<<6)|((uint64_t)((f>>3)&1)<<7)|((uint64_t)((f>>4)&1)<<11);
"#);
    for i in &p.instructions {
        writeln!(c, "{{rosemary_machine m={{0}};m.r[1]=counts[j];m.rip={};m.cf=initial&1;m.pf=(initial>>2)&1;m.zf=(initial>>6)&1;m.sf=(initial>>7)&1;m.of=(initial>>11)&1;assert(rosemary_run(&m,0)==1&&m.r[1]==counts[j]);assert(rosemary_run(&m,1)==1);uint64_t counter=counts[j],flags;unsigned taken;", i.address).unwrap();
        writeln!(c, "__asm__ volatile(\"pushq %3;popfq;{} 1f;movl $0,%0;jmp 2f;1:movl $1,%0;2:pushfq;popq %1\":\"=&r\"(taken),\"=&r\"(flags),\"+c\"(counter):\"r\"(initial):\"cc\",\"memory\");assert(m.r[1]==counter&&m.rip==(taken?0x1000:{}));", i.op, i.address+1).unwrap();
        c.push_str("assert(m.cf==(flags&1)&&m.pf==((flags>>2)&1)&&m.zf==((flags>>6)&1)&&m.sf==((flags>>7)&1)&&m.of==((flags>>11)&1));\n");
        writeln!(c, "uint64_t rebuilt,rebuilt_flags,rebuilt_count=counts[j];__asm__ volatile(\"pushq %3;popfq;call long_entry_{};pushfq;popq %1\":\"=a\"(rebuilt),\"=&r\"(rebuilt_flags),\"+c\"(rebuilt_count):\"r\"(initial):\"cc\",\"memory\");assert(rebuilt==taken&&rebuilt_count==counter&&((rebuilt_flags^flags)&0x8c5)==0);}}", i.address).unwrap();
    }
    c.push_str("}return 0;}\n");
    let dir = std::env::temp_dir().join(format!("rosemary-counter-native-{}", std::process::id()));
    fs::create_dir_all(&dir).unwrap();
    fs::write(dir.join("native.c"), c).unwrap();
    let code = decode::decode(
        &input::hex("e2 fe e1 fe e0 fe e3 fe c3").unwrap(),
        0x1000,
        None,
    )
    .unwrap();
    let mut assembly = emit::assembly(&code).unwrap();
    for (n, op) in ["loop", "loope", "loopne", "jrcxz"].into_iter().enumerate() {
        let mut long = Program {
            entry: 0,
            instructions: vec![Instruction {
                address: 0,
                len: 2,
                op: op.into(),
                operands: vec![Operand::Imm(208, 64)],
            }],
        };
        for address in 2..202 {
            long.instructions.push(Instruction {
                address,
                len: 1,
                op: "nop".into(),
                operands: vec![],
            });
        }
        long.instructions.extend(Program::from_text("rosemary-ir 1\nentry ca\nca 5 mov r:32:0 i:32:0\ncf 1 ret\nd0 5 mov r:32:0 i:32:1\nd5 1 ret\n").unwrap().instructions);
        assembly.push_str(
            &emit::assembly(&long)
                .unwrap()
                .replace("rosemary_entry", &format!("long_entry_{n}"))
                .replace(".L_", &format!(".L_long_{n}_")),
        );
    }
    assembly.push_str(".section .note.GNU-stack,\"\",@progbits\n");
    fs::write(dir.join("native.s"), assembly).unwrap();
    let output = Command::new("cc")
        .args([
            "-std=c11",
            "-O2",
            "-mno-red-zone",
            "-Wall",
            "-Wextra",
            "-Werror",
        ])
        .arg(dir.join("native.c"))
        .arg(dir.join("native.s"))
        .arg("-o")
        .arg(dir.join("native"))
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(Command::new(dir.join("native")).status().unwrap().success());
    fs::remove_dir_all(dir).unwrap();
}
