#![cfg(target_arch = "x86_64")]
use rosemary::{
    decode, emit, input,
    ir::{CONDITIONS, Instruction, Operand, Program},
};
use std::{fmt::Write, fs, process::Command};

struct Case {
    instruction: Instruction,
    native: String,
    width: u8,
    flags: &'static str,
}
fn add(
    cases: &mut Vec<Case>,
    op: &str,
    operands: Vec<Operand>,
    native: String,
    width: u8,
    flags: &'static str,
) {
    cases.push(Case {
        instruction: Instruction {
            address: cases.len() as u64,
            len: 1,
            op: op.into(),
            operands,
        },
        native,
        width,
        flags,
    });
}
#[test]
fn narrow_registers_shifts_and_all_conditional_moves_match_native_x86() {
    let mut cases = Vec::new();
    for (width, suffix, reg) in [
        (8, "b", "b"),
        (16, "w", "w"),
        (32, "l", "k"),
        (64, "q", "q"),
    ] {
        for op in ["shl", "shr", "sar", "rol", "ror", "rcl", "rcr"] {
            add(
                &mut cases,
                op,
                vec![Operand::Reg(0, width), Operand::Reg(1, 8)],
                format!("{op}{suffix} %%cl, %{reg}0"),
                width,
                if matches!(op, "rcl" | "rcr") {
                    "carry_rotate"
                } else if matches!(op, "rol" | "ror") {
                    "rotate"
                } else {
                    "shift"
                },
            );
        }
        for op in [
            "add", "adc", "sub", "sbb", "xor", "and", "or", "cmp", "test",
        ] {
            add(
                &mut cases,
                op,
                vec![Operand::Reg(0, width), Operand::Reg(2, width)],
                format!("{op}{suffix} %{reg}2, %{reg}0"),
                width,
                "all",
            );
        }
        for op in ["inc", "dec", "neg", "not"] {
            add(
                &mut cases,
                op,
                vec![Operand::Reg(0, width)],
                format!("{op}{suffix} %{reg}0"),
                width,
                "all",
            );
        }
    }
    for condition in CONDITIONS {
        let suffix = &condition[1..];
        for (width, size, reg) in [(16, "w", "w"), (32, "l", "k"), (64, "q", "q")] {
            add(
                &mut cases,
                &format!("cmov{suffix}"),
                vec![Operand::Reg(0, width), Operand::Reg(2, width)],
                format!("cmov{suffix}{size} %{reg}2, %{reg}0"),
                width,
                "all",
            );
        }
        add(
            &mut cases,
            &format!("set{suffix}"),
            vec![Operand::Reg(0, 8)],
            format!("set{suffix} %b0"),
            8,
            "all",
        );
    }
    for (op, src, dst, native) in [
        ("movzx", 8, 16, "movzbw %b2, %w0"),
        ("movzx", 8, 32, "movzbl %b2, %k0"),
        ("movzx", 8, 64, "movzbq %b2, %q0"),
        ("movzx", 16, 32, "movzwl %w2, %k0"),
        ("movzx", 16, 64, "movzwq %w2, %q0"),
        ("movsx", 8, 16, "movsbw %b2, %w0"),
        ("movsx", 8, 32, "movsbl %b2, %k0"),
        ("movsx", 8, 64, "movsbq %b2, %q0"),
        ("movsx", 16, 32, "movswl %w2, %k0"),
        ("movsx", 16, 64, "movswq %w2, %q0"),
    ] {
        add(
            &mut cases,
            op,
            vec![Operand::Reg(0, dst), Operand::Reg(2, src)],
            native.into(),
            dst,
            "all",
        );
    }
    let p = Program {
        entry: 0,
        instructions: cases.iter().map(|c| c.instruction.clone()).collect(),
    };
    let mut c = emit::c(&p).unwrap();
    c.push_str(
        r#"
#include <assert.h>
int main(void) {
    uint64_t values[]={0,1,2,7,8,15,16,31,32,63,64,65,127,128,255,256,
        0x7fff,0x8000,0x7fffffff,0x80000000,0xffffffff,
        UINT64_C(0x7fffffffffffffff),UINT64_C(0x8000000000000000),UINT64_MAX};
    for(unsigned j=0;j<sizeof(values)/sizeof(values[0]);j++)
    for(unsigned k=0;k<sizeof(values)/sizeof(values[0]);k++)
    for(unsigned f=0;f<32;f++) {
        uint64_t a=values[j], b=values[k];
        uint64_t initial=2 | (f&1) | ((uint64_t)((f>>1)&1)<<2) | ((uint64_t)((f>>2)&1)<<6)
            | ((uint64_t)((f>>3)&1)<<7) | ((uint64_t)((f>>4)&1)<<11);
"#,
    );
    for case in &cases {
        let address = case.instruction.address;
        writeln!(c,"{{ rosemary_machine m={{0}}; m.r[0]=a; m.r[1]=b; m.r[2]=b; m.rip={address}; m.cf=initial&1; m.pf=(initial>>2)&1; m.zf=(initial>>6)&1; m.sf=(initial>>7)&1; m.of=(initial>>11)&1; assert(rosemary_run(&m,1)==1); uint64_t expected=a, flags;").unwrap();
        writeln!(c,"__asm__ volatile(\"pushq %3; popfq; {}; pushfq; popq %1\" : \"+a\"(expected), \"=r\"(flags) : \"d\"(b), \"r\"(initial), \"c\"(b) : \"cc\", \"memory\");",case.native).unwrap();
        c.push_str("assert(m.r[0]==expected);\n");
        if case.flags == "carry_rotate" {
            writeln!(c, "unsigned masked=(unsigned)b&{};unsigned effective={};assert(m.cf==(flags&1)&&m.pf==((flags>>2)&1)&&m.zf==((flags>>6)&1)&&m.sf==((flags>>7)&1));if(!effective||masked==1)assert(m.of==((flags>>11)&1));", if case.width==64 {63}else{31}, if case.width<32 {format!("masked%{}",case.width+1)} else {"masked".into()}).unwrap();
        } else if matches!(case.flags, "shift" | "rotate") {
            writeln!(c,"unsigned count=(unsigned)b & {}; assert(m.pf==((flags>>2)&1) && m.zf==((flags>>6)&1) && m.sf==((flags>>7)&1));",if case.width==64 {63}else{31}).unwrap();
            writeln!(c,"if(!count || count<{} || {}) assert(m.cf==(flags&1));\nif(count<2) assert(m.of==((flags>>11)&1));",case.width,if case.instruction.op=="sar" {1}else{0}).unwrap();
        } else {
            c.push_str("assert(m.cf==(flags&1) && m.of==((flags>>11)&1) && m.pf==((flags>>2)&1) && m.zf==((flags>>6)&1) && m.sf==((flags>>7)&1));\n");
        }
        c.push_str("}\n");
    }
    c.push_str("} return 0; }\n");
    let dir = std::env::temp_dir().join(format!("rosemary-extended-native-{}", std::process::id()));
    fs::create_dir_all(&dir).unwrap();
    fs::write(dir.join("native.c"), c).unwrap();
    let output = Command::new("cc")
        .args([
            "-O2",
            "-std=c11",
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
    let output = Command::new("cc")
        .arg("-c")
        .arg(dir.join("native.s"))
        .arg("-o")
        .arg(dir.join("native.o"))
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    fs::remove_dir_all(dir).unwrap();
}
#[test]
fn decoder_covers_narrow_conditional_and_high_byte_encodings() {
    let high = decode::decode(&input::hex("88 e0 b4 01 0f b6 c4").unwrap(), 0, None).unwrap();
    assert_eq!(high.instructions[0].operands[1], Operand::High(0));
    assert_eq!(high.instructions[1].operands[0], Operand::High(0));
    let p=decode::decode(&input::hex("0f b6 03 48 0f b7 03 0f be 03 48 0f bf 03 66 b8 34 12 40 b4 01 b0 02 80 c0 01 66 83 e8 01 c1 e0 03 48 d3 f8 d0 e8 0f 43 c2 0f 95 c0 66 ff c0 c6 03 7f").unwrap(),0,None).unwrap();
    assert_eq!(
        p.instructions
            .iter()
            .map(|i| i.op.as_str())
            .collect::<Vec<_>>(),
        [
            "movzx", "movzx", "movsx", "movsx", "mov", "mov", "mov", "add", "sub", "shl", "sar",
            "shr", "cmovae", "setne", "inc", "mov"
        ]
    );
    assert_eq!(p.instructions[5].operands[0], Operand::Reg(4, 8));
    assert_eq!(p.instructions[4].operands[1], Operand::Imm(0x1234, 16));
    for hex in ["66 50", "66 ff d0", "f0 c1 d0 01"] {
        assert!(
            decode::decode(&input::hex(hex).unwrap(), 0, None).is_err(),
            "{hex}"
        );
    }
}
