use rosemary::{
    dataflow::{self, Flags, State},
    decode, emit, input,
    ir::{Operand, Program, Segment},
};
#[cfg(target_arch = "x86_64")]
use std::{fmt::Write, fs, process::Command};

const OPS: [(&str, u8, u8, u8); 3] = [
    ("movddup", 0xf2, 0x12, 64),
    ("movsldup", 0xf3, 0x12, 128),
    ("movshdup", 0xf3, 0x16, 128),
];

#[test]
fn duplicate_moves_decode_registers_and_exact_memory_widths() {
    let state = State {
        registers: std::array::from_fn(|n| Some(n as u64 * 17)),
        flags: Flags {
            zf: Some(true),
            sf: Some(false),
            cf: Some(true),
            of: Some(false),
            pf: Some(true),
        },
        direction: Some(true),
    };
    for (op, prefix, opcode, width) in OPS {
        for dst in 0..16 {
            for src in 0..16 {
                for rex_w in [0, 8] {
                    let rex = 0x40 | rex_w | ((dst >> 3) << 2) | (src >> 3);
                    let bytes = [
                        prefix,
                        rex,
                        0x0f,
                        opcode,
                        0xc0 | ((dst & 7) << 3) | (src & 7),
                    ];
                    let p = decode::decode(&bytes, 0x1000, None).unwrap();
                    let i = &p.instructions[0];
                    assert_eq!(i.op, op);
                    assert_eq!(i.operands, [Operand::Vec(dst), Operand::Vec(src)]);
                    assert_eq!(usize::from(i.len), bytes.len());
                    assert_eq!(Program::from_text(&p.to_text().unwrap()).unwrap(), p);
                    assert_eq!(dataflow::transfer(i, state), state);
                }
            }
        }
        // FS:[r12+r13*2-16], writing XMM8.
        let bytes = [0x64, prefix, 0x47, 0x0f, opcode, 0x44, 0x6c, 0xf0];
        let p = decode::decode(&bytes, 0x1000, None).unwrap();
        assert_eq!(
            p.instructions[0].operands,
            [
                Operand::Vec(8),
                Operand::Mem {
                    segment: Some(Segment::Fs),
                    width,
                    base: Some(12),
                    index: Some(13),
                    scale: 2,
                    disp: (-16i64) as u64,
                }
            ]
        );
        assert_eq!(dataflow::transfer(&p.instructions[0], state), state);
        let assembly = emit::assembly(&p).unwrap();
        assert!(assembly.contains(if width == 64 {
            "qword ptr"
        } else {
            "xmmword ptr"
        }));
        let bytes = [prefix, 0x44, 0x0f, opcode, 0x05, 0xf0, 0xff, 0xff, 0xff];
        let p = decode::decode(&bytes, 0x1000, None).unwrap();
        assert_eq!(
            p.instructions[0].operands,
            [
                Operand::Vec(8),
                Operand::Mem {
                    segment: None,
                    width,
                    base: None,
                    index: None,
                    scale: 1,
                    disp: 0xff9,
                }
            ]
        );
        assert_eq!(p.instructions[0].len, 9);
        for size in 0..bytes.len() {
            assert!(
                decode::decode(&bytes[..size], 0, None).is_err(),
                "{op} {size}"
            );
        }
    }
}

#[test]
fn duplicate_moves_reject_invalid_operands_and_prefix_combinations() {
    for (op, _, _, width) in OPS {
        for operands in [
            "x:128:0 r:64:0".to_owned(),
            "m:128:0:-:1:0 x:128:1".to_owned(),
            "x:128:0".to_owned(),
            "x:128:0 x:128:1 i:8:0".to_owned(),
            format!("x:128:0 m:{}:0:-:1:0", if width == 64 { 128 } else { 64 }),
        ] {
            assert!(
                Program::from_text(&format!("rosemary-ir 1\nentry 0\n0 1 {op} {operands}\n"))
                    .is_err(),
                "{op} {operands}"
            );
        }
    }
    for encoding in [
        "f2 0f 16 c1",
        "f2 0f 13 01",
        "f3 0f 17 01",
        "66 f2 0f 12 c1",
        "66 f3 0f 12 c1",
        "f0 f2 0f 12 01",
    ] {
        assert!(
            decode::decode(&input::hex(encoding).unwrap(), 0, None).is_err(),
            "{encoding}"
        );
    }
}

#[test]
#[cfg(target_arch = "x86_64")]
fn duplicate_moves_match_native_bits_aliases_and_memory_accesses() {
    let mut bytes = Vec::new();
    for (_, prefix, opcode, _) in OPS {
        bytes.extend([prefix, 0x0f, opcode, 0xc1]); // xmm0, xmm1
        bytes.extend([prefix, 0x0f, opcode, 0xc0]); // xmm0, xmm0
        bytes.extend([prefix, 0x45, 0x0f, opcode, 0xc1]); // xmm8, xmm9
        bytes.extend([prefix, 0x0f, opcode, 0x03]); // xmm0, [rbx]
        bytes.extend([0x64, prefix, 0x47, 0x0f, opcode, 0x44, 0x6c, 0xf0]);
    }
    let p = decode::decode(&bytes, 0x1000, None).unwrap();
    let mut c = emit::c(&p).unwrap();
    c.push_str(r#"
#include <emmintrin.h>
#include <assert.h>
#include <string.h>
typedef struct { uint64_t words[2]; unsigned loads; } memory;
static uint64_t load(void *ctx,uint64_t a,unsigned n) {
 memory *mem=ctx;assert(n==8&&a==128+8*mem->loads&&mem->loads<2);
 return mem->words[mem->loads++];
}
int main(void) {
#if defined(__GNUC__) || defined(__clang__)
 if(!__builtin_cpu_supports("sse3")) return 77;
#endif
 uint64_t seed=17;
 const uint64_t special[][2]={
  {0,UINT64_MAX},{UINT64_C(0x8000000000000000),0},
  {UINT64_C(0x7ff0000000000001),UINT64_C(0xfff8000000001234)},
  {UINT64_C(0x7f80000180000000),UINT64_C(0x7fc01234ff800001)},
  {UINT64_C(0x0000000100000000),UINT64_C(0x8000000100000001)},
  {UINT64_C(0x0123456789abcdef),UINT64_C(0xfedcba9876543210)}
 };
 for(unsigned trial=0;trial<512;trial++) {
  uint64_t aa[2],bb[2];
  for(unsigned j=0;j<2;j++){seed=seed*UINT64_C(6364136223846793005)+1;aa[j]=seed;seed=seed*UINT64_C(6364136223846793005)+1;bb[j]=seed;}
  if(trial<sizeof(special)/sizeof(special[0])) memcpy(bb,special[trial],16);
"#);
    for i in &p.instructions {
        let Operand::Vec(dst) = i.operands[0] else {
            unreachable!()
        };
        let alias = i.operands[1] == i.operands[0];
        let memory = matches!(i.operands[1], Operand::Mem { .. });
        writeln!(c, "{{memory mem={{{{bb[0],bb[1]}},0}};rosemary_machine m={{0}};for(unsigned j=0;j<16;j++){{m.r[j]=j*17;m.x[j]=(rosemary_vec){{aa[0],aa[1]}};}}m.r[3]=128;m.r[12]=80;m.r[13]=16;m.fs_base=32;m.ctx=&mem;m.load=load;m.rip={};", i.address).unwrap();
        if let Operand::Vec(src) = i.operands[1] {
            writeln!(c, "m.x[{src}]=(rosemary_vec){{bb[0],bb[1]}};").unwrap();
        }
        c.push_str("rosemary_vec before[16];uint64_t registers[16];memcpy(before,m.x,sizeof(before));memcpy(registers,m.r,sizeof(registers));unsigned flag=trial&1;m.cf=m.zf=m.sf=m.of=m.pf=m.df=flag;__m128i expected;memcpy(&expected,aa,16);assert(rosemary_run(&m,0)==1);assert(mem.loads==0);assert(rosemary_run(&m,1)==1);");
        if alias {
            c.push_str("memcpy(&expected,bb,16);");
            writeln!(c, "__asm__ volatile(\"{} %0,%0\":\"+x\"(expected));", i.op).unwrap();
        } else if memory {
            writeln!(
                c,
                "__asm__ volatile(\"{} %1,%0\":\"=x\"(expected):\"m\"(bb):\"memory\");",
                i.op
            )
            .unwrap();
        } else {
            c.push_str("__m128i source;memcpy(&source,bb,16);");
            writeln!(
                c,
                "__asm__ volatile(\"{} %1,%0\":\"=x\"(expected):\"x\"(source));",
                i.op
            )
            .unwrap();
        }
        let loads = if memory {
            i.operands[1].width() / 64
        } else {
            0
        };
        writeln!(c, "uint64_t words[2];memcpy(words,&expected,16);assert(m.x[{dst}].lo==words[0]&&m.x[{dst}].hi==words[1]);for(unsigned j=0;j<16;j++) if(j!={dst}) assert(memcmp(&m.x[j],&before[j],sizeof(before[j]))==0);assert(mem.loads=={loads}&&mem.words[0]==bb[0]&&mem.words[1]==bb[1]);assert(memcmp(m.r,registers,sizeof(registers))==0&&m.cf==flag&&m.zf==flag&&m.sf==flag&&m.of==flag&&m.pf==flag&&m.df==flag);assert(m.rip=={});}}",i.address+u64::from(i.len)).unwrap();
    }
    c.push_str("}return 0;}\n");
    let dir =
        std::env::temp_dir().join(format!("rosemary-vector-duplicates-{}", std::process::id()));
    fs::create_dir_all(&dir).unwrap();
    fs::write(dir.join("native.c"), c).unwrap();
    let output = Command::new("cc")
        .args(["-std=c11", "-O2", "-msse2", "-Wall", "-Wextra", "-Werror"])
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
    let status = Command::new(dir.join("native")).status().unwrap();
    if status.code() == Some(77) {
        eprintln!("Native SSE3 execution skipped: CPU lacks SSE3");
    } else {
        assert!(status.success());
    }
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
