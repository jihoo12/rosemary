#![cfg(target_arch = "x86_64")]
use rosemary::{
    decode, emit, input,
    ir::{Instruction, Operand, Program},
};
use std::{fmt::Write, fs, process::Command};

struct Case {
    instruction: Instruction,
    native: String,
    kind: u8,
}
fn add(cases: &mut Vec<Case>, op: &str, operands: Vec<Operand>, native: String, kind: u8) {
    cases.push(Case {
        instruction: Instruction {
            address: cases.len() as u64,
            len: 1,
            op: op.into(),
            operands,
        },
        native,
        kind,
    });
}
#[test]
fn sse2_generated_c_matches_native_register_and_memory_operations() {
    let mut cases = Vec::new();
    for op in [
        "movups",
        "movaps",
        "movupd",
        "movapd",
        "movdqu",
        "movdqa",
        "pxor",
        "xorps",
        "xorpd",
        "pand",
        "pandn",
        "por",
        "andps",
        "andnps",
        "orps",
        "andpd",
        "andnpd",
        "orpd",
        "paddb",
        "paddw",
        "paddd",
        "paddq",
        "psubb",
        "psubw",
        "psubd",
        "psubq",
        "pcmpeqb",
        "pcmpeqw",
        "pcmpeqd",
        "pcmpgtb",
        "pcmpgtw",
        "pcmpgtd",
        "pminub",
        "pmaxub",
        "pminsw",
        "pmaxsw",
        "punpcklbw",
        "punpcklwd",
        "punpckldq",
        "punpcklqdq",
        "punpckhbw",
        "punpckhwd",
        "punpckhdq",
        "punpckhqdq",
        "pmuludq",
        "psllw",
        "pslld",
        "psllq",
        "psrlw",
        "psrld",
        "psrlq",
        "psraw",
        "psrad",
        "movlhps",
        "movhlps",
        "movss",
        "movsd",
    ] {
        add(
            &mut cases,
            op,
            vec![Operand::Vec(0), Operand::Vec(1)],
            format!("{op} %1,%0"),
            0,
        );
    }
    for op in ["pshufd", "pshuflw", "pshufhw"] {
        for imm in 0..=255 {
            add(
                &mut cases,
                op,
                vec![Operand::Vec(0), Operand::Vec(1), Operand::Imm(imm, 8)],
                format!("{op} ${imm},%1,%0"),
                0,
            );
        }
    }
    for op in [
        "psllw", "pslld", "psllq", "psrlw", "psrld", "psrlq", "psraw", "psrad", "pslldq", "psrldq",
    ] {
        for imm in [0, 1, 7, 8, 15, 16, 31, 32, 63, 64, 127, 128, 255] {
            add(
                &mut cases,
                op,
                vec![Operand::Vec(0), Operand::Imm(imm, 8)],
                format!("{op} ${imm},%0"),
                0,
            );
        }
    }
    for (op, width) in [("movd", 32), ("movq", 64)] {
        add(
            &mut cases,
            op,
            vec![Operand::Vec(0), Operand::Reg(2, width)],
            format!("{op} %{}1,%0", if width == 32 { "k" } else { "q" }),
            1,
        );
        add(
            &mut cases,
            op,
            vec![Operand::Reg(0, width), Operand::Vec(1)],
            format!("{op} %1,%{}0", if width == 32 { "k" } else { "q" }),
            2,
        );
    }
    add(
        &mut cases,
        "movq",
        vec![Operand::Vec(0), Operand::Vec(1)],
        "movq %1,%0".into(),
        0,
    );
    for op in ["pmovmskb", "movmskps", "movmskpd"] {
        add(
            &mut cases,
            op,
            vec![Operand::Reg(0, 32), Operand::Vec(1)],
            format!("{op} %1,%k0"),
            2,
        );
    }
    for imm in [0, 1, 3, 7, 8, 255] {
        add(
            &mut cases,
            "pinsrw",
            vec![Operand::Vec(0), Operand::Reg(2, 32), Operand::Imm(imm, 8)],
            format!("pinsrw ${imm},%k1,%0"),
            1,
        );
        add(
            &mut cases,
            "pextrw",
            vec![Operand::Reg(0, 32), Operand::Vec(1), Operand::Imm(imm, 8)],
            format!("pextrw ${imm},%1,%k0"),
            2,
        );
    }
    for op in [
        "movups", "movaps", "movupd", "movapd", "movdqu", "movdqa", "pxor", "pandn", "paddq",
        "pcmpgtb", "movss", "movsd", "pminub", "pmaxub", "pminsw", "pmaxsw",
    ] {
        let width = match op {
            "movss" => 32,
            "movsd" => 64,
            _ => 128,
        };
        add(
            &mut cases,
            op,
            vec![
                Operand::Vec(0),
                Operand::Mem {
                    segment: None,
                    width,
                    base: Some(3),
                    index: None,
                    scale: 1,
                    disp: 0,
                },
            ],
            format!("{op} %1,%0"),
            3,
        );
    }
    let p = Program {
        entry: 0,
        instructions: cases.iter().map(|c| c.instruction.clone()).collect(),
    };
    let mut c = emit::c(&p).unwrap();
    c.push_str(r#"
#include <emmintrin.h>
#include <assert.h>
#include <string.h>
_Alignas(16) static uint64_t memory[2];
static uint64_t load(void *ctx,uint64_t addr,unsigned n){
    (void)ctx; assert(addr>=128 && addr+n<=144); uint64_t v=0;
    memcpy(&v,(unsigned char*)memory+addr-128,n);return v;
}
int main(void){
    uint64_t seed=37;
    for(unsigned trial=0;trial<192;trial++){
        uint64_t aa[2],bb[2];
        for(unsigned j=0;j<2;j++){seed=seed*UINT64_C(6364136223846793005)+1;aa[j]=seed;seed=seed*UINT64_C(6364136223846793005)+1;bb[j]=seed;}
        if(trial<4){aa[0]=aa[1]=trial%2?UINT64_MAX:0;bb[0]=bb[1]=trial<2?aa[0]:~aa[0];}
        else if(trial==4){aa[0]=aa[1]=UINT64_C(0x80007fff0000ffff);bb[0]=bb[1]=UINT64_C(0x7fff8000ffff0000);}
        else if(trial==5){aa[0]=aa[1]=UINT64_C(0x80ff7f0080ff7f00);bb[0]=bb[1]=UINT64_C(0x7f0080ff7f0080ff);}
        else if(trial%3==0) bb[0]=trial-4;
        __m128i va,vb;memcpy(&va,aa,16);memcpy(&vb,bb,16);memcpy(memory,bb,16);
"#);
    for case in &cases {
        writeln!(c,"{{rosemary_machine m={{0}};m.x[0]=(rosemary_vec){{aa[0],aa[1]}};m.x[1]=(rosemary_vec){{bb[0],bb[1]}};m.r[0]=aa[0];m.r[2]=bb[0];m.r[3]=128;m.load=load;m.rip={};m.cf=m.of=m.zf=m.sf=m.pf=1;assert(rosemary_run(&m,1)==1);",case.instruction.address).unwrap();
        match case.kind {
            0 | 1 | 3 => {
                let constraint = match case.kind {
                    1 => "\"r\"(bb[0])",
                    3 => "\"m\"(memory)",
                    _ => "\"x\"(vb)",
                };
                writeln!(c,"__m128i expected=va;__asm__ volatile(\"{}\":\"+x\"(expected):{constraint}:\"memory\");uint64_t words[2];memcpy(words,&expected,16);assert(m.x[0].lo==words[0]&&m.x[0].hi==words[1]);",case.native).unwrap();
            }
            _ => {
                writeln!(c,"uint64_t expected=aa[0];__asm__ volatile(\"{}\":\"+a\"(expected):\"x\"(vb):\"memory\");assert(m.r[0]==expected);",case.native).unwrap();
            }
        }
        c.push_str("assert(m.cf&&m.of&&m.zf&&m.sf&&m.pf);}\n");
    }
    c.push_str("}return 0;}\n");
    let dir = std::env::temp_dir().join(format!("rosemary-vector-native-{}", std::process::id()));
    fs::create_dir_all(&dir).unwrap();
    fs::write(dir.join("vector.c"), c).unwrap();
    let result = Command::new("cc")
        .args(["-std=c11", "-O2", "-msse2", "-Wall", "-Wextra", "-Werror"])
        .arg(dir.join("vector.c"))
        .arg("-o")
        .arg(dir.join("vector"))
        .output()
        .unwrap();
    assert!(
        result.status.success(),
        "{}",
        String::from_utf8_lossy(&result.stderr)
    );
    assert!(Command::new(dir.join("vector")).status().unwrap().success());
    fs::write(dir.join("vector.s"), emit::assembly(&p).unwrap()).unwrap();
    let result = Command::new("cc")
        .arg("-c")
        .arg(dir.join("vector.s"))
        .arg("-o")
        .arg(dir.join("vector.o"))
        .output()
        .unwrap();
    assert!(
        result.status.success(),
        "{}",
        String::from_utf8_lossy(&result.stderr)
    );
    fs::remove_dir_all(dir).unwrap();
}
#[test]
fn vector_ir_roundtrip_and_encoding_validation() {
    let p=decode::decode(&input::hex("0f 10 03 0f 11 03 66 0f ef c0 f3 0f 6f 03 66 48 0f 6e c0 f3 0f 7e c1 66 0f 70 c1 1b f2 0f 70 c1 1b 66 0f 73 d0 08 66 0f d7 c0 66 0f c4 c0 03").unwrap(),0,None).unwrap();
    assert_eq!(Program::from_text(&p.to_text().unwrap()).unwrap(), p);
    assert_eq!(p.instructions[0].operands[0], Operand::Vec(0));
    assert!(emit::assembly(&p).unwrap().contains("xmmword ptr"));
    for line in [
        "movups r:64:0 x:128:0",
        "pshufd x:128:0 x:128:1 i:64:0",
        "mov r:128:0 i:64:0",
        "mov r:64:0 m:128:0:-:1:0",
    ] {
        assert!(
            Program::from_text(&format!("rosemary-ir 1\nentry 0\n0 1 {line}\n")).is_err(),
            "{line}"
        );
    }
}
