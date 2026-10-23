use rosemary::{
    dataflow::{self, Flags, State},
    decode,
    dynamic::{self, Machine, Options, Stop},
    emit,
    ir::{Operand, Program, Segment},
};
#[cfg(target_arch = "x86_64")]
use std::{fmt::Write, fs, process::Command};

const OPS: [(&str, u8, u8); 4] = [
    ("pminsb", 0x38, 8),
    ("pminsd", 0x39, 32),
    ("pminuw", 0x3a, 16),
    ("pminud", 0x3b, 32),
];

#[test]
fn minmax_decoding_preserves_widths_registers_segments_and_static_flags() {
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
    for (op, opcode, _) in OPS {
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

fn repeat(bits: u8, values: [u64; 4]) -> u128 {
    (0..128 / bits).fold(0, |v, n| {
        v | (u128::from(values[usize::from(n) % 4]) << (n * bits))
    })
}

#[test]
fn minmax_signedness_selects_original_lane_bit_patterns_without_flag_changes() {
    for (op, opcode, bits) in OPS {
        let sign = 1u64 << (bits - 1);
        let mask = (1u64 << bits) - 1;
        let p = decode::decode(&[0x66, 0x0f, 0x38, opcode, 0xc1], 0x1000, None).unwrap();
        let mut m = Machine::with_stack(p.entry);
        m.state.xmm[0] = repeat(bits, [sign, mask, sign - 1, 0]);
        m.state.xmm[1] = repeat(bits, [0, 1, sign + 1, mask - 1]);
        m.state.flags.cf = true;
        m.state.flags.of = true;
        m.state.flags.df = true;
        m.state.mxcsr = 0x5fc0;
        let before = m.state;
        let result = dynamic::execute(&p, &mut m, Options::default()).unwrap();
        assert_eq!(result.stop, Stop::LeftCode);
        let mut expected = before;
        expected.rip += u64::from(p.instructions[0].len);
        expected.xmm[0] = if matches!(opcode, 0x38 | 0x39 | 0x3e | 0x3f) {
            repeat(bits, [sign, mask, sign + 1, mask - 1])
        } else {
            repeat(bits, [0, 1, sign - 1, 0])
        };
        assert_eq!(m.state, expected, "{op}");
    }
}

#[test]
fn minmax_memory_reads_fault_atomically_and_resume_with_original_state() {
    for (op, opcode, _) in OPS {
        let p = decode::decode(&[0x66, 0x44, 0x0f, 0x38, opcode, 0x03], 0x1000, None).unwrap();
        let mut m = Machine::with_stack(p.entry);
        m.state.registers[3] = 0x2000;
        m.state.flags.cf = true;
        m.state.flags.of = true;
        m.state.flags.df = true;
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
        expected.xmm[8] = if matches!(opcode, 0x38 | 0x39 | 0x3e | 0x3f) {
            u128::MAX
        } else {
            0
        };
        assert_eq!(m.state, expected, "{op}");
    }
}

#[cfg(target_arch = "x86_64")]
#[target_feature(enable = "sse4.1")]
unsafe fn native_minmax(opcode: u8, left: u128, right: u128) -> u128 {
    use std::arch::x86_64::{__m128i, _mm_min_epi8, _mm_min_epi32, _mm_min_epu16, _mm_min_epu32};
    let left = unsafe { std::mem::transmute::<u128, __m128i>(left) };
    let right = unsafe { std::mem::transmute::<u128, __m128i>(right) };
    let result = match opcode {
        0x38 => _mm_min_epi8(left, right),
        0x39 => _mm_min_epi32(left, right),
        0x3a => _mm_min_epu16(left, right),
        _ => _mm_min_epu32(left, right),
    };
    unsafe { std::mem::transmute::<__m128i, u128>(result) }
}

#[test]
#[cfg(target_arch = "x86_64")]
fn concrete_minmax_matches_native_for_all_byte_pairs_words_and_dword_boundaries() {
    if !std::arch::is_x86_feature_detected!("sse4.1") {
        return;
    }
    let partners = [0, 1, 0xffff, 0x8000, 0x7fff, 0x8001, 0x4000, 0xc000];
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
    for (op, opcode, bits) in OPS {
        for mode in 0..3 {
            let bytes = if mode == 2 {
                vec![0x65, 0x66, 0x47, 0x0f, 0x38, opcode, 0x44, 0x6c, 0xf0]
            } else {
                vec![
                    0x66,
                    0x45,
                    0x0f,
                    0x38,
                    opcode,
                    if mode == 1 { 0xc0 } else { 0xc1 },
                ]
            };
            let p = decode::decode(&bytes, 0x1000, None).unwrap();
            let mut m = Machine::with_stack(p.entry);
            m.state.gs_base = 0x2020;
            m.state.registers[12] = 80;
            m.state.registers[13] = 16;
            m.state.xmm.fill(u128::MAX);
            m.state.mxcsr = 0x5fc0;
            m.memory.map(0x2080, vec![0; 16], true).unwrap();
            let trials = if bits == 16 { 131072 } else { 65536 };
            let mask = (1u64 << bits) - 1;
            for trial in 0..trials {
                let mut left = 0;
                let mut right = 0;
                for lane in 0..128 / bits {
                    seed = seed.wrapping_mul(6364136223846793005).wrapping_add(1);
                    let a_seed = seed >> 32;
                    seed = seed.wrapping_mul(6364136223846793005).wrapping_add(1);
                    let b_seed = seed >> 32;
                    let (a, b) = match bits {
                        8 => (
                            (trial + u64::from(lane) * 31) & mask,
                            ((trial >> 8) + u64::from(lane) * 17) & mask,
                        ),
                        16 => (
                            (trial + u64::from(lane) * 8191) & mask,
                            if trial < 65536 {
                                partners[usize::from(lane)]
                            } else {
                                b_seed & mask
                            },
                        ),
                        _ => {
                            if trial < 64 {
                                (
                                    boundary[(trial as usize / 8 + usize::from(lane)) % 8],
                                    boundary[(trial as usize % 8 + 3 * usize::from(lane)) % 8],
                                )
                            } else {
                                (a_seed, b_seed)
                            }
                        }
                    };
                    left |= u128::from(a) << (lane * bits);
                    right |= u128::from(b) << (lane * bits);
                }
                if mode == 1 {
                    right = left;
                }
                m.state.rip = p.entry;
                m.state.xmm[8] = left;
                m.state.xmm[9] = right;
                m.state.flags.cf = trial & 1 != 0;
                m.state.flags.pf = trial & 2 != 0;
                m.state.flags.zf = trial & 4 != 0;
                m.state.flags.sf = trial & 8 != 0;
                m.state.flags.of = trial & 16 != 0;
                m.state.flags.df = trial & 32 != 0;
                m.memory.store(0x2080, 8, right as u64).unwrap();
                m.memory.store(0x2088, 8, (right >> 64) as u64).unwrap();
                let before = m.state;
                // Runtime feature detection above protects the SSE4.1 reference.
                let value = unsafe { native_minmax(opcode, left, right) };
                let report = dynamic::execute(
                    &p,
                    &mut m,
                    Options {
                        max_steps: 1,
                        trace_limit: 0,
                    },
                )
                .unwrap();
                assert_eq!(report.stop, Stop::LeftCode);
                let mut expected = before;
                expected.rip += u64::from(p.instructions[0].len);
                expected.xmm[8] = value;
                assert_eq!(m.state, expected, "{op}, mode {mode}, trial {trial}");
                assert_eq!(
                    (report.memory_reads, report.memory_writes),
                    (if mode == 2 { 2 } else { 0 }, 0)
                );
                assert_eq!(m.memory.load(0x2080, 8).unwrap(), right as u64);
                assert_eq!(m.memory.load(0x2088, 8).unwrap(), (right >> 64) as u64);
            }
        }
    }
}

#[test]
#[cfg(target_arch = "x86_64")]
fn c_minmax_matches_native_for_lane_selection_aliases_and_segmented_memory() {
    if !std::arch::is_x86_feature_detected!("sse4.1") {
        return;
    }
    let mut bytes = Vec::new();
    for (_, opcode, _) in OPS {
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
typedef struct{uint64_t words[2];unsigned loads;} memory;
static uint64_t load(void *ctx,uint64_t a,unsigned n){memory *m=ctx;assert(n==8&&m->loads<2&&a==128+8*m->loads);return m->words[m->loads++];}
int main(void){const uint64_t boundary[]={0,UINT64_MAX,UINT64_C(0x8080808080808080),UINT64_C(0x7f7f7f7f7f7f7f7f),UINT64_C(0x8000800080008000),UINT64_C(0x7fff7fff7fff7fff),UINT64_C(0x8000000080000000),UINT64_C(0x7fffffff7fffffff)};uint64_t seed=73;
 for(unsigned trial=0;trial<65536;trial++){
  _Alignas(16) uint64_t aa[2],bb[2];
  for(unsigned j=0;j<2;j++){seed=seed*UINT64_C(6364136223846793005)+1;aa[j]=seed;seed=seed*UINT64_C(6364136223846793005)+1;bb[j]=seed;if(trial<64){aa[j]=boundary[(trial/8+j)%8];bb[j]=boundary[(trial%8+3*j)%8];}}
"#);
    for i in &p.instructions {
        let Operand::Vec(dst) = i.operands[0] else {
            unreachable!()
        };
        let alias = i.operands[0] == i.operands[1];
        let memory = matches!(i.operands[1], Operand::Mem { .. });
        writeln!(c,"{{memory mem={{{{bb[0],bb[1]}},0}};rosemary_machine m={{0}};for(unsigned j=0;j<16;j++){{m.r[j]=j*17;m.x[j]=(rosemary_vec){{UINT64_C(0x0123456789abcdef),UINT64_C(0xfedcba9876543210)}};}}m.x[0]=m.x[8]=(rosemary_vec){{aa[0],aa[1]}};m.x[1]=m.x[9]=(rosemary_vec){{bb[0],bb[1]}};m.r[3]=128;m.r[12]=80;m.r[13]=16;m.fs_base=m.gs_base=32;m.mxcsr=0x5fc0;m.ctx=&mem;m.load=load;m.rip={};m.cf=trial&1;m.pf=(trial>>1)&1;m.zf=(trial>>2)&1;m.sf=(trial>>3)&1;m.of=(trial>>4)&1;m.df=(trial>>5)&1;rosemary_machine before=m;assert(rosemary_run(&m,1)==1&&m.rip=={});__m128i expected;memcpy(&expected,aa,16);",i.address,i.address+u64::from(i.len)).unwrap();
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
        writeln!(c,"uint64_t words[2];memcpy(words,&expected,16);assert(m.x[{dst}].lo==words[0]&&m.x[{dst}].hi==words[1]);for(unsigned j=0;j<16;j++)if(j!={dst})assert(memcmp(&m.x[j],&before.x[j],sizeof(m.x[j]))==0);assert(mem.loads=={}&&memcmp(mem.words,bb,16)==0&&memcmp(m.r,before.r,sizeof(m.r))==0&&m.mxcsr==before.mxcsr&&m.fs_base==before.fs_base&&m.gs_base==before.gs_base);assert(m.cf==before.cf&&m.pf==before.pf&&m.zf==before.zf&&m.sf==before.sf&&m.of==before.of&&m.df==before.df);}}",if memory{2}else{0}).unwrap();
    }
    c.push_str("}return 0;}\n");
    let dir = std::env::temp_dir().join(format!("rosemary-minmax-{}", std::process::id()));
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
    fs::remove_dir_all(dir).unwrap();
}
