use rosemary::{
    dataflow::{self, Flags, State},
    decode, emit, input,
    ir::{Instruction, Operand, Program},
};
#[cfg(target_arch = "x86_64")]
use std::{fmt::Write, fs, process::Command};

#[test]
fn carry_instructions_decode_and_reject_invalid_operand_shapes() {
    let p = decode::decode(&input::hex("f8 f9 f5 d0 d0 d0 dc d1 d0 66 d1 d8 48 d3 d0 41 c1 d8 ff 40 d2 d4 48 c1 53 f8 01 64 d3 1b").unwrap(), 0, None).unwrap();
    assert_eq!(
        p.instructions
            .iter()
            .map(|i| i.op.as_str())
            .collect::<Vec<_>>(),
        [
            "clc", "stc", "cmc", "rcl", "rcr", "rcl", "rcr", "rcl", "rcr", "rcl", "rcl", "rcr"
        ]
    );
    assert_eq!(p.instructions[4].operands[0], Operand::High(0));
    assert_eq!(p.instructions[8].operands[0], Operand::Reg(8, 32));
    assert_eq!(p.instructions[9].operands[0], Operand::Reg(4, 8));
    assert_eq!(Program::from_text(&p.to_text().unwrap()).unwrap(), p);
    for line in [
        "clc i:8:1",
        "stc r:64:0",
        "cmc r:8:1",
        "rcl r:32:0 i:16:1",
        "rcr r:64:0 r:8:0",
        "rcl i:8:1 i:8:1",
        "lock.rcr m:64:0:-:1:0 i:8:1",
    ] {
        assert!(
            Program::from_text(&format!("rosemary-ir 1\nentry 0\n0 1 {line}\n")).is_err(),
            "{line}"
        );
    }
    for hex in ["c0 d0", "48 c1 d8", "f0 f8", "f3 f9", "f0 d0 10"] {
        assert!(
            decode::decode(&input::hex(hex).unwrap(), 0, None).is_err(),
            "{hex}"
        );
    }
}

fn instruction(op: &str, width: u8, count: u64) -> Instruction {
    Instruction {
        address: 0,
        len: 1,
        op: op.into(),
        operands: vec![Operand::Reg(0, width), Operand::Imm(count, 8)],
    }
}

#[test]
fn carry_transfer_preserves_unaffected_flags_and_forgets_undefined_overflow() {
    let mut before = State {
        flags: Flags {
            cf: Some(true),
            of: Some(true),
            zf: Some(true),
            sf: Some(false),
            pf: Some(true),
        },
        direction: Some(true),
        ..Default::default()
    };
    before.registers[0] = Some(0x123456789abcdef0);
    for op in ["rcl", "rcr"] {
        for (width, count) in [(8, 9), (8, 18), (16, 17), (32, 32), (64, 64)] {
            let after = dataflow::transfer(&instruction(op, width, count), before);
            let mut expected = before;
            if width == 32 {
                expected.registers[0] = Some(0x9abcdef0);
            }
            assert_eq!(after, expected, "{op} {width} {count}");
        }
        let after = dataflow::transfer(&instruction(op, 8, 10), before);
        assert_eq!(after.flags.of, None); // Masked count is 10, effective count is 1.
        assert_eq!(after.flags.zf, before.flags.zf);
        assert_eq!(after.flags.sf, before.flags.sf);
        assert_eq!(after.flags.pf, before.flags.pf);
        assert_eq!(after.direction, before.direction);
        let mut unknown = before;
        unknown.flags.cf = None;
        let after = dataflow::transfer(&instruction(op, 8, 1), unknown);
        assert_eq!(after.registers[0], None);
        assert_eq!(after.flags.cf, Some(op == "rcl")); // 0xf0 has MSB=1, LSB=0.
        assert_eq!(after.flags.of, None);
    }
    for (op, carry) in [
        ("clc", Some(false)),
        ("stc", Some(true)),
        ("cmc", Some(false)),
    ] {
        let i = Instruction {
            address: 0,
            len: 1,
            op: op.into(),
            operands: vec![],
        };
        let mut expected = before;
        expected.flags.cf = carry;
        assert_eq!(dataflow::transfer(&i, before), expected);
    }
    let p = Program::from_text(
        "rosemary-ir 1\nentry 0\n0 1 stc\n1 1 cmc\n2 1 jb i:64:4\n3 1 ret\n4 1 ret\n",
    )
    .unwrap();
    let a = dataflow::analyze(&p).unwrap();
    assert_eq!(a.branch(2), Some(false));
    assert_eq!(a.simplify().unwrap().instructions[2].op, "nop");
}

#[test]
#[cfg(target_arch = "x86_64")]
fn memory_carry_rotates_match_native_for_all_cl_counts_and_incoming_flags() {
    let mut p = Program {
        entry: 0,
        instructions: vec![],
    };
    for op in ["rcl", "rcr"] {
        for width in [8, 16, 32, 64] {
            p.instructions.push(Instruction {
                address: p.instructions.len() as u64,
                len: 1,
                op: op.into(),
                operands: vec![
                    Operand::Mem {
                        segment: None,
                        width,
                        base: Some(3),
                        index: None,
                        scale: 1,
                        disp: 0,
                    },
                    Operand::Reg(1, 8),
                ],
            });
        }
    }
    for op in ["clc", "stc", "cmc"] {
        p.instructions.push(Instruction {
            address: p.instructions.len() as u64,
            len: 1,
            op: op.into(),
            operands: vec![],
        });
    }
    let mut c = emit::c(&p).unwrap();
    c.push_str(r#"
#include <assert.h>
#include <string.h>
static uint64_t load(void *ctx, uint64_t a, unsigned n) { assert(a==128); uint64_t v=0; memcpy(&v,ctx,n); return v; }
static void store(void *ctx, uint64_t a, unsigned n, uint64_t v) { assert(a==128); memcpy(ctx,&v,n); }
int main(void) {
 const uint64_t values[]={0,1,0x80,0x8000,0x80000000,UINT64_C(0x8000000000000000),UINT64_C(0x123456789abcdef0),UINT64_MAX};
 for(unsigned j=0;j<sizeof(values)/sizeof(values[0]);j++)
 for(unsigned count=0;count<256;count++)for(unsigned f=0;f<32;f++) {
 uint64_t initial=2|(f&1)|((uint64_t)((f>>1)&1)<<2)|((uint64_t)((f>>2)&1)<<6)|((uint64_t)((f>>3)&1)<<7)|((uint64_t)((f>>4)&1)<<11);
"#);
    for i in &p.instructions {
        writeln!(c, "{{uint64_t memory=values[j], reference=memory, flags;rosemary_machine m={{0}};m.r[1]=count;m.r[3]=128;m.ctx=&memory;m.load=load;m.store=store;m.rip={};m.cf=initial&1;m.pf=(initial>>2)&1;m.zf=(initial>>6)&1;m.sf=(initial>>7)&1;m.of=(initial>>11)&1;assert(rosemary_run(&m,1)==1);", i.address).unwrap();
        if let Some(dst) = i.operands.first() {
            let w = dst.width();
            let suffix = match w {
                8 => "b",
                16 => "w",
                32 => "l",
                _ => "q",
            };
            writeln!(c, "__asm__ volatile(\"pushq %3;popfq;{}{} %%cl,%0;pushfq;popq %1\":\"+m\"(reference),\"=r\"(flags):\"c\"(count),\"r\"(initial):\"cc\",\"memory\");unsigned masked=count&{};unsigned effective={};assert(memory==reference&&m.cf==(flags&1));if(!effective||masked==1)assert(m.of==((flags>>11)&1));", i.op, suffix, if w==64 {63} else {31}, if w<32 {format!("masked%{}",w+1)} else {"masked".into()}).unwrap();
        } else {
            writeln!(c, "__asm__ volatile(\"pushq %1;popfq;{};pushfq;popq %0\":\"=r\"(flags):\"r\"(initial):\"cc\",\"memory\");assert(memory==reference&&m.cf==(flags&1)&&m.of==((flags>>11)&1));", i.op).unwrap();
        }
        c.push_str("assert(m.zf==((flags>>6)&1)&&m.sf==((flags>>7)&1)&&m.pf==((flags>>2)&1));}\n");
    }
    c.push_str("}return 0;}\n");
    let dir = std::env::temp_dir().join(format!("rosemary-carry-native-{}", std::process::id()));
    fs::create_dir_all(&dir).unwrap();
    fs::write(dir.join("native.c"), c).unwrap();
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
    fs::write(dir.join("native.s"), emit::assembly(&p).unwrap()).unwrap();
    assert!(
        Command::new("cc")
            .arg("-c")
            .arg(dir.join("native.s"))
            .arg("-o")
            .arg(dir.join("native.o"))
            .status()
            .unwrap()
            .success()
    );
    fs::remove_dir_all(dir).unwrap();
}
