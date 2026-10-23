use rosemary::{
    dataflow::{self, Flags, State},
    decode,
    dynamic::{self, Machine, Options, Stop},
    emit,
    ir::{Operand, Program, Segment},
};
#[cfg(target_arch = "x86_64")]
use std::{fmt::Write, fs, process::Command};

const OPS: [(&str, u8); 2] = [("pmulld", 0x40), ("pmuldq", 0x28)];

#[test]
fn dword_products_preserve_register_extensions_segments_and_ir_validation() {
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
    for (op, opcode) in OPS {
        for dst in 0..16 {
            for src in 0..16 {
                for w in [0, 8] {
                    let bytes = [
                        0x66,
                        0x40 | w | ((dst >> 3) << 2) | (src >> 3),
                        0x0f,
                        0x38,
                        opcode,
                        0xc0 | ((dst & 7) << 3) | (src & 7),
                    ];
                    let p = decode::decode(&bytes, 0x1000, None).unwrap();
                    let i = &p.instructions[0];
                    assert_eq!(i.op, op);
                    assert_eq!(i.len as usize, bytes.len());
                    assert_eq!(i.operands, [Operand::Vec(dst), Operand::Vec(src)]);
                    assert_eq!(Program::from_text(&p.to_text().unwrap()).unwrap(), p);
                    assert_eq!(dataflow::transfer(i, state), state);
                }
            }
        }
        for (prefix, segment) in [(0x64, Segment::Fs), (0x65, Segment::Gs)] {
            let p = decode::decode(
                &[prefix, 0x66, 0x47, 0x0f, 0x38, opcode, 0x44, 0x6c, 0xf0],
                0x1000,
                None,
            )
            .unwrap();
            assert_eq!(
                p.instructions[0].operands,
                [
                    Operand::Vec(8),
                    Operand::Mem {
                        segment: Some(segment),
                        width: 128,
                        base: Some(12),
                        index: Some(13),
                        scale: 2,
                        disp: (-16i64) as u64
                    }
                ]
            );
            assert!(
                emit::assembly(&p)
                    .unwrap()
                    .contains(&format!("xmmword ptr {}:", segment.name()))
            );
        }
        let bytes = [0x66, 0x44, 0x0f, 0x38, opcode, 0x05, 0xf0, 0xff, 0xff, 0xff];
        let p = decode::decode(&bytes, 0x1000, None).unwrap();
        assert!(
            matches!(p.instructions[0].operands[1],Operand::Mem{width:128,base:None,disp,..} if disp==0x1000+bytes.len() as u64-16)
        );
        for size in 0..bytes.len() {
            assert!(decode::decode(&bytes[..size], 0x1000, None).is_err());
        }
        for prefix in [
            vec![],
            vec![0xf2],
            vec![0xf3],
            vec![0xf0, 0x66],
            vec![0xf3, 0x66],
        ] {
            let mut bytes = prefix;
            bytes.extend([0x0f, 0x38, opcode, 0xc1]);
            assert!(decode::decode(&bytes, 0x1000, None).is_err());
        }
        for operands in [
            "m:128:0:-:1:0 x:128:1",
            "x:128:0 m:64:0:-:1:0",
            "x:128:0 r:64:0",
            "x:128:0 i:8:1",
            "x:128:0",
            "x:128:0 x:128:1 i:8:0",
        ] {
            assert!(
                Program::from_text(&format!(
                    "rosemary-ir 1\nentry 1000\n1000 1 {op} {operands}\n"
                ))
                .is_err()
            );
        }
    }
}

#[test]
fn low_dword_products_wrap_signed_extremes_without_saturation() {
    let p = decode::decode(&[0x66, 0x0f, 0x38, 0x40, 0xc1], 0x1000, None).unwrap();
    let mut m = Machine::with_stack(p.entry);
    m.state.xmm[0] = 0x40000001_ffffffff_7fffffff_80000000;
    m.state.xmm[1] = 0x00000004_ffffffff_7fffffff_ffffffff;
    m.state.flags.cf = true;
    m.state.flags.of = true;
    m.state.flags.df = true;
    m.state.mxcsr = 0x5fc0;
    let before = m.state;
    assert_eq!(
        dynamic::execute(&p, &mut m, Options::default())
            .unwrap()
            .stop,
        Stop::LeftCode
    );
    let mut expected = before;
    expected.xmm[0] = 0x00000004_00000001_00000001_80000000;
    expected.rip += u64::from(p.instructions[0].len);
    assert_eq!(m.state, expected);
}

#[test]
fn signed_widening_products_ignore_odd_lanes_and_capture_aliases() {
    for alias in [false, true] {
        let p = decode::decode(
            &[0x66, 0x0f, 0x38, 0x28, if alias { 0xc0 } else { 0xc1 }],
            0x1000,
            None,
        )
        .unwrap();
        for poison in [0u32, 1, 0x7fff_ffff, 0x8000_0000, u32::MAX] {
            let mut m = Machine::with_stack(p.entry);
            m.state.xmm[0] = 0x00000000_7fffffff_00000000_80000000
                | (u128::from(poison) << 32)
                | (u128::from(!poison) << 96);
            m.state.xmm[1] = 0x00000000_80000000_00000000_80000000
                | (u128::from(!poison) << 32)
                | (u128::from(poison) << 96);
            m.state.flags.cf = true;
            m.state.flags.of = true;
            m.state.flags.df = true;
            m.state.mxcsr = 0x5fc0;
            let before = m.state;
            assert_eq!(
                dynamic::execute(&p, &mut m, Options::default())
                    .unwrap()
                    .stop,
                Stop::LeftCode
            );
            let mut expected = before;
            expected.xmm[0] = if alias {
                0x3fffffff00000001_4000000000000000
            } else {
                0xc000000080000000_4000000000000000
            };
            expected.rip += u64::from(p.instructions[0].len);
            assert_eq!(m.state, expected, "alias {alias}, odd lane bits {poison:x}");
        }
    }
}

#[test]
fn zero_dword_multipliers_read_full_memory_and_fault_without_partial_updates() {
    for (_, opcode) in OPS {
        let p = decode::decode(&[0x66, 0x44, 0x0f, 0x38, opcode, 0x03], 0x1000, None).unwrap();
        let mut m = Machine::with_stack(p.entry);
        m.state.registers[3] = 0x2000;
        m.memory.map(0x2000, vec![255; 8], false).unwrap();
        let before = m.state;
        let report = dynamic::execute(&p, &mut m, Options::default()).unwrap();
        assert!(matches!(report.stop, Stop::MemoryFault(_)));
        assert_eq!(m.state, before);
        assert_eq!((report.memory_reads, report.memory_writes), (1, 0));
        assert!(!report.trace[0].completed);
        m.memory.map(0x2008, vec![255; 8], false).unwrap();
        let report = dynamic::execute(&p, &mut m, Options::default()).unwrap();
        assert_eq!(report.stop, Stop::LeftCode);
        assert_eq!((report.memory_reads, report.memory_writes), (2, 0));
        let mut expected = before;
        expected.rip += u64::from(p.instructions[0].len);
        assert_eq!(m.state, expected);
    }
}

#[test]
#[cfg(target_arch = "x86_64")]
fn generated_c_and_concrete_dword_products_match_native_boundaries_aliases_and_memory() {
    if !std::arch::is_x86_feature_detected!("sse4.1") {
        return;
    }
    let mut bytes = Vec::new();
    for (_, opcode) in OPS {
        for (prefix, suffix) in [
            (&[0x66][..], &[0xc1][..]),
            (&[0x66][..], &[0xc0][..]),
            (&[0x66, 0x45][..], &[0xc1][..]),
            (&[0x66][..], &[0x03][..]),
            (&[0x64, 0x66, 0x47][..], &[0x44, 0x6c, 0xf0][..]),
            (&[0x65, 0x66, 0x47][..], &[0x44, 0x6c, 0xf0][..]),
        ] {
            bytes.extend(prefix);
            bytes.extend([0x0f, 0x38, opcode]);
            bytes.extend(suffix);
        }
    }
    let p = decode::decode(&bytes, 0x1000, None).unwrap();
    let mut c = emit::c(&p).unwrap();
    c.push_str(r#"
#include <emmintrin.h>
#include <assert.h>
#include <string.h>
#include <stdio.h>
#include <inttypes.h>
typedef struct{uint64_t words[2];unsigned loads;} memory;
static uint64_t load(void *ctx,uint64_t a,unsigned n){memory *m=ctx;assert(n==8&&m->loads<2&&a==128+8*m->loads);return m->words[m->loads++];}
int main(void){
 const uint32_t boundary[]={0,1,0xffffffff,0x7fffffff,0x80000000,0x80000001,0x0000ffff,0xffff0000};
 uint64_t seed=73;
 for(unsigned trial=0;trial<65536;trial++){
  _Alignas(16) uint64_t aa[2]={0},bb[2]={0};
  for(unsigned j=0;j<4;j++){
   seed=seed*UINT64_C(6364136223846793005)+1;uint32_t a=(uint32_t)(seed>>32);
   seed=seed*UINT64_C(6364136223846793005)+1;uint32_t b=(uint32_t)(seed>>32);
   if(trial<64){a=boundary[(trial/8+j)%8];b=boundary[(trial%8+3*j)%8];}
   aa[j/2]|=(uint64_t)a<<(32*(j%2));bb[j/2]|=(uint64_t)b<<(32*(j%2));
  }
"#);
    for i in &p.instructions {
        let Operand::Vec(dst) = i.operands[0] else {
            unreachable!()
        };
        let alias = i.operands[0] == i.operands[1];
        let memory = matches!(i.operands[1], Operand::Mem { .. });
        writeln!(c,"{{memory mem={{{{bb[0],bb[1]}},0}};rosemary_machine m={{0}};for(unsigned j=0;j<16;j++){{m.r[j]=j*17;m.x[j]=(rosemary_vec){{UINT64_C(0x0123456789abcdef),UINT64_C(0xfedcba9876543210)}};}}m.x[0]=m.x[8]=(rosemary_vec){{aa[0],aa[1]}};m.x[1]=m.x[9]=(rosemary_vec){{bb[0],bb[1]}};m.r[3]=128;m.r[12]=80;m.r[13]=16;m.fs_base=m.gs_base=32;m.mxcsr=0x5fc0;m.ctx=&mem;m.load=load;m.rip={};m.cf=trial&1;m.pf=(trial>>1)&1;m.zf=(trial>>2)&1;m.sf=(trial>>3)&1;m.of=(trial>>4)&1;m.df=(trial>>5)&1;rosemary_machine before=m;__m128i expected;memcpy(&expected,aa,16);assert(rosemary_run(&m,1)==1&&m.rip=={});",i.address,i.address+u64::from(i.len)).unwrap();
        if alias {
            writeln!(c, "__asm__ volatile(\"{} %0,%0\":\"+x\"(expected));", i.op).unwrap();
        } else if memory {
            writeln!(
                c,
                "__asm__ volatile(\"{} %1,%0\":\"+x\"(expected):\"m\"(bb):\"memory\");",
                i.op
            )
            .unwrap();
        } else {
            writeln!(c,"__m128i source;memcpy(&source,bb,16);__asm__ volatile(\"{} %1,%0\":\"+x\"(expected):\"x\"(source));",i.op).unwrap();
        }
        writeln!(c,"uint64_t words[2];memcpy(words,&expected,16);assert(m.x[{dst}].lo==words[0]&&m.x[{dst}].hi==words[1]);for(unsigned j=0;j<16;j++)if(j!={dst})assert(memcmp(&m.x[j],&before.x[j],sizeof(m.x[j]))==0);assert(mem.loads=={}&&memcmp(mem.words,bb,16)==0&&memcmp(m.r,before.r,sizeof(m.r))==0&&m.mxcsr==before.mxcsr&&m.fs_base==before.fs_base&&m.gs_base==before.gs_base);assert(m.cf==before.cf&&m.pf==before.pf&&m.zf==before.zf&&m.sf==before.sf&&m.of==before.of&&m.df==before.df);printf(\"%\" PRIx64 \" %\" PRIx64 \"\\n\",words[0],words[1]);}}",if memory{2}else{0}).unwrap();
    }
    c.push_str("}return 0;}\n");
    let boundary = [
        0,
        1,
        0xffff_ffff,
        0x7fff_ffff,
        0x8000_0000,
        0x8000_0001,
        0x0000_ffff,
        0xffff_0000,
    ];
    let mut seed = 73u64;
    let mut expected = String::new();
    for trial in 0..65536usize {
        let mut left = 0;
        let mut right = 0;
        for lane in 0..4 {
            seed = seed.wrapping_mul(6364136223846793005).wrapping_add(1);
            let mut a = seed >> 32;
            seed = seed.wrapping_mul(6364136223846793005).wrapping_add(1);
            let mut b = seed >> 32;
            if trial < 64 {
                a = boundary[(trial / 8 + lane) % 8];
                b = boundary[(trial % 8 + 3 * lane) % 8];
            }
            left |= u128::from(a) << (32 * lane);
            right |= u128::from(b) << (32 * lane);
        }
        for i in &p.instructions {
            let mut m = Machine::default();
            m.state.rip = i.address;
            m.state.mxcsr = 0x5fc0;
            m.state.fs_base = 32;
            m.state.gs_base = 32;
            for (n, r) in m.state.registers.iter_mut().enumerate() {
                *r = n as u64 * 17;
            }
            m.state.xmm.fill(0xfedcba9876543210_0123456789abcdef);
            m.state.xmm[0] = left;
            m.state.xmm[8] = left;
            m.state.xmm[1] = right;
            m.state.xmm[9] = right;
            m.state.registers[3] = 128;
            m.state.registers[12] = 80;
            m.state.registers[13] = 16;
            m.state.flags.cf = trial & 1 != 0;
            m.state.flags.pf = trial & 2 != 0;
            m.state.flags.zf = trial & 4 != 0;
            m.state.flags.sf = trial & 8 != 0;
            m.state.flags.of = trial & 16 != 0;
            m.state.flags.df = trial & 32 != 0;
            m.memory
                .map(128, right.to_le_bytes().to_vec(), false)
                .unwrap();
            let before = m.state;
            let report = dynamic::execute(
                &p,
                &mut m,
                Options {
                    max_steps: 1,
                    trace_limit: 0,
                },
            )
            .unwrap();
            assert!(matches!(report.stop, Stop::Budget | Stop::LeftCode));
            let Operand::Vec(dst) = i.operands[0] else {
                unreachable!()
            };
            let value = m.state.xmm[dst as usize];
            let mut after = before;
            after.rip += u64::from(i.len);
            after.xmm[dst as usize] = value;
            assert_eq!(m.state, after);
            assert_eq!(
                (report.memory_reads, report.memory_writes),
                (
                    if matches!(i.operands[1], Operand::Mem { .. }) {
                        2
                    } else {
                        0
                    },
                    0
                )
            );
            writeln!(expected, "{:x} {:x}", value as u64, value >> 64).unwrap();
        }
    }
    let dir = std::env::temp_dir().join(format!("rosemary-dword-products-{}", std::process::id()));
    fs::create_dir_all(&dir).unwrap();
    fs::write(dir.join("test.c"), c).unwrap();
    fs::write(dir.join("test.s"), emit::assembly(&p).unwrap()).unwrap();
    for (source, output, flags) in [
        (
            "test.c",
            "test",
            vec![
                "-std=c11",
                "-O1",
                "-msse2",
                "-fsanitize=undefined",
                "-fno-sanitize-recover=all",
                "-Wall",
                "-Wextra",
                "-Werror",
            ],
        ),
        ("test.s", "test.o", vec!["-c"]),
    ] {
        let result = Command::new("cc")
            .args(flags)
            .arg(dir.join(source))
            .arg("-o")
            .arg(dir.join(output))
            .output()
            .unwrap();
        assert!(
            result.status.success(),
            "{}",
            String::from_utf8_lossy(&result.stderr)
        );
    }
    let result = Command::new(dir.join("test")).output().unwrap();
    assert!(
        result.status.success(),
        "{}",
        String::from_utf8_lossy(&result.stderr)
    );
    let actual = String::from_utf8(result.stdout).unwrap();
    assert_eq!(actual.lines().count(), expected.lines().count());
    for (n, (actual, expected)) in actual.lines().zip(expected.lines()).enumerate() {
        assert_eq!(
            actual,
            expected,
            "{:?}, trial {}",
            p.instructions[n % p.instructions.len()],
            n / p.instructions.len()
        );
    }
    fs::remove_dir_all(dir).unwrap();
}
