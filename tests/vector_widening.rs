use rosemary::{
    dataflow::{self, Flags, State},
    decode,
    dynamic::{self, Machine, Options, Stop},
    emit,
    ir::{Operand, Program, Segment},
};
#[cfg(all(target_arch = "x86_64", unix))]
use std::{fmt::Write, fs, process::Command};

// Mnemonic, opcode, input lane bits, output lane bits, memory source bits.
const OPS: [(&str, u8, u8, u8, u8); 12] = [
    ("pmovsxbw", 0x20, 8, 16, 64),
    ("pmovsxbd", 0x21, 8, 32, 32),
    ("pmovsxbq", 0x22, 8, 64, 16),
    ("pmovsxwd", 0x23, 16, 32, 64),
    ("pmovsxwq", 0x24, 16, 64, 32),
    ("pmovsxdq", 0x25, 32, 64, 64),
    ("pmovzxbw", 0x30, 8, 16, 64),
    ("pmovzxbd", 0x31, 8, 32, 32),
    ("pmovzxbq", 0x32, 8, 64, 16),
    ("pmovzxwd", 0x33, 16, 32, 64),
    ("pmovzxwq", 0x34, 16, 64, 32),
    ("pmovzxdq", 0x35, 32, 64, 64),
];

#[test]
fn widening_decodes_all_registers_and_exact_memory_widths_and_preserves_static_state() {
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
    for (op, opcode, _, _, width) in OPS {
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
                        width,
                        base: Some(12),
                        index: Some(13),
                        scale: 2,
                        disp: (-16i64) as u64
                    }
                ]
            );
            assert_eq!(dataflow::transfer(&p.instructions[0], state), state);
            assert_eq!(Program::from_text(&p.to_text().unwrap()).unwrap(), p);
            let size = match width {
                16 => "word",
                32 => "dword",
                _ => "qword",
            };
            assert!(
                emit::assembly(&p)
                    .unwrap()
                    .contains(&format!("{op} xmm8, {size} ptr {}:", segment.name()))
            );
        }
        let bytes = [0x66, 0x44, 0x0f, 0x38, opcode, 0x05, 0xf0, 0xff, 0xff, 0xff];
        let p = decode::decode(&bytes, 0x1000, None).unwrap();
        assert!(
            matches!(p.instructions[0].operands[1],Operand::Mem {width:w,base:None,disp,..} if w==width && disp==0x1000+bytes.len() as u64-16)
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
        for invalid_width in [8, 16, 32, 64, 128] {
            if invalid_width != width {
                assert!(
                    Program::from_text(&format!(
                        "rosemary-ir 1\nentry 1000\n1000 1 {op} x:128:0 m:{invalid_width}:0:-:1:0\n"
                    ))
                    .is_err()
                );
            }
        }
    }
    for op in ["pmovsxb", "pmovsxbx", "pmovsxdb", "pmovzxqq"] {
        assert!(
            Program::from_text(&format!(
                "rosemary-ir 1\nentry 1000\n1000 1 {op} x:128:0 x:128:1\n"
            ))
            .is_err()
        );
    }
}

fn reference(source: u128, input: u8, output: u8, signed: bool) -> u128 {
    let modulus = 1i128 << input;
    let mask = (1u128 << output) - 1;
    (0..128 / output).fold(0, |result, n| {
        let raw = ((source >> (n * input)) & ((1u128 << input) - 1)) as i128;
        let value = if signed && raw >= modulus / 2 {
            raw - modulus
        } else {
            raw
        };
        result | ((value as u128 & mask) << (n * output))
    })
}

#[test]
fn widening_overwrites_every_destination_lane_and_ignores_unused_source_bits() {
    for (op, opcode, input, output, width) in OPS {
        let sign = 1u128 << (input - 1);
        let mask = (1u128 << input) - 1;
        let source = (0..128 / input).fold(0, |v, n| {
            v | ([sign, mask, sign - 1, 0][usize::from(n) % 4] << (n * input))
        });
        for alias in [false, true] {
            let p = decode::decode(
                &[
                    0x66,
                    0x45,
                    0x0f,
                    0x38,
                    opcode,
                    if alias { 0xc0 } else { 0xc1 },
                ],
                0x1000,
                None,
            )
            .unwrap();
            for upper in [0, u128::MAX] {
                let mut m = Machine::with_stack(p.entry);
                m.state.xmm.fill(u128::MAX);
                m.state.xmm[if alias { 8 } else { 9 }] =
                    (source & ((1u128 << width) - 1)) | (upper & !((1u128 << width) - 1));
                m.state.flags = dynamic::Flags {
                    cf: true,
                    zf: true,
                    sf: true,
                    of: true,
                    pf: true,
                    df: true,
                };
                m.state.mxcsr = 0x5fc0;
                let before = m.state;
                let report = dynamic::execute(&p, &mut m, Options::default()).unwrap();
                assert_eq!(report.stop, Stop::LeftCode);
                let mut expected = before;
                expected.rip += u64::from(p.instructions[0].len);
                expected.xmm[8] = reference(source, input, output, opcode < 0x30);
                assert_eq!(m.state, expected, "{op} alias {alias}");
                assert_eq!((report.memory_reads, report.memory_writes), (0, 0));
            }
        }
    }
}

#[test]
fn widening_split_read_faults_atomically_then_resumes_without_reading_past_the_source() {
    for (op, opcode, input, output, width) in OPS {
        let p = decode::decode(
            &[0x65, 0x66, 0x47, 0x0f, 0x38, opcode, 0x44, 0x6c, 0xf0],
            0x1000,
            None,
        )
        .unwrap();
        let mut m = Machine::with_stack(p.entry);
        m.state.xmm.fill(u128::MAX);
        m.state.gs_base = 0x2020;
        m.state.registers[12] = 80;
        m.state.registers[13] = 16;
        m.state.flags.cf = true;
        m.state.flags.of = true;
        m.state.flags.df = true;
        m.state.mxcsr = 0x5fc0;
        let bytes = width / 8;
        let source = (0..bytes).map(|n| 0x80 + n).collect::<Vec<_>>();
        m.memory
            .map(0x2080, source[..usize::from(bytes) - 1].to_vec(), false)
            .unwrap();
        let before = m.state;
        let report = dynamic::execute(&p, &mut m, Options::default()).unwrap();
        assert!(matches!(report.stop, Stop::MemoryFault(_)));
        assert_eq!(m.state, before);
        assert_eq!((report.memory_reads, report.memory_writes), (0, 0));
        assert!(!report.trace[0].completed);
        m.memory
            .map(
                0x2080 + u64::from(bytes) - 1,
                vec![source[usize::from(bytes) - 1]],
                false,
            )
            .unwrap();
        let report = dynamic::execute(&p, &mut m, Options::default()).unwrap();
        assert_eq!(report.stop, Stop::LeftCode);
        assert_eq!((report.memory_reads, report.memory_writes), (1, 0));
        assert_eq!(report.trace[0].accesses.len(), 1);
        assert_eq!(report.trace[0].accesses[0].bytes, bytes);
        assert_eq!(report.trace[0].accesses[0].address, 0x2080);
        let raw = source
            .iter()
            .enumerate()
            .fold(0u128, |v, (n, b)| v | (u128::from(*b) << (8 * n)));
        let mut expected = before;
        expected.rip += u64::from(p.instructions[0].len);
        expected.xmm[8] = reference(raw, input, output, opcode < 0x30);
        assert_eq!(m.state, expected, "{op}");
        assert_eq!(u128::from(m.memory.load(0x2080, bytes).unwrap()), raw);
    }
}

#[cfg(target_arch = "x86_64")]
#[target_feature(enable = "sse4.1")]
unsafe fn native_widen(opcode: u8, source: u128) -> u128 {
    use std::arch::x86_64::*;
    let source = unsafe { std::mem::transmute::<u128, __m128i>(source) };
    let result = match opcode {
        0x20 => _mm_cvtepi8_epi16(source),
        0x21 => _mm_cvtepi8_epi32(source),
        0x22 => _mm_cvtepi8_epi64(source),
        0x23 => _mm_cvtepi16_epi32(source),
        0x24 => _mm_cvtepi16_epi64(source),
        0x25 => _mm_cvtepi32_epi64(source),
        0x30 => _mm_cvtepu8_epi16(source),
        0x31 => _mm_cvtepu8_epi32(source),
        0x32 => _mm_cvtepu8_epi64(source),
        0x33 => _mm_cvtepu16_epi32(source),
        0x34 => _mm_cvtepu16_epi64(source),
        _ => _mm_cvtepu32_epi64(source),
    };
    unsafe { std::mem::transmute::<__m128i, u128>(result) }
}

#[test]
#[cfg(target_arch = "x86_64")]
fn concrete_widening_matches_native_for_byte_and_word_sweeps_and_dword_boundaries() {
    if !std::arch::is_x86_feature_detected!("sse4.1") {
        return;
    }
    let boundary = [
        0,
        1,
        0x7fff_ffff,
        0x8000_0000,
        0x8000_0001,
        0xffff_ffff,
        0x0000_ffff,
        0xffff_0000,
    ];
    let mut seed = 73u64;
    for (op, opcode, input, output, width) in OPS {
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
            m.state.xmm.fill(u128::MAX);
            m.state.gs_base = 0x2020;
            m.state.registers[12] = 80;
            m.state.registers[13] = 16;
            m.state.mxcsr = 0x5fc0;
            m.memory
                .map(0x2080, vec![0; usize::from(width / 8)], true)
                .unwrap();
            for trial in 0..65536u64 {
                let mut source = 0u128;
                for n in 0..128 / input {
                    seed = seed.wrapping_mul(6364136223846793005).wrapping_add(1);
                    let value = if n >= 128 / output {
                        seed >> 32
                    } else if input < 32 {
                        trial + u64::from(n) * 8191
                    } else if trial < 64 {
                        boundary[(trial as usize / 8 + usize::from(n) * 3) % 8]
                    } else {
                        seed >> 32
                    };
                    source |= u128::from(value & ((1u64 << input) - 1)) << (n * input);
                }
                m.state.rip = p.entry;
                m.state.xmm[8] = if mode == 1 { source } else { !source };
                m.state.xmm[9] = source;
                m.state.flags = dynamic::Flags {
                    cf: trial & 1 != 0,
                    pf: trial & 2 != 0,
                    zf: trial & 4 != 0,
                    sf: trial & 8 != 0,
                    of: trial & 16 != 0,
                    df: trial & 32 != 0,
                };
                m.memory.store(0x2080, width / 8, source as u64).unwrap();
                let before = m.state;
                // Runtime feature detection above protects this CPU reference.
                let value = unsafe { native_widen(opcode, source) };
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
                assert_eq!(m.state, expected, "{op} mode {mode} trial {trial}");
                assert_eq!(
                    (report.memory_reads, report.memory_writes),
                    (u64::from(mode == 2), 0)
                );
                assert_eq!(
                    m.memory.load(0x2080, width / 8).unwrap(),
                    source as u64 & ((1u128 << width) - 1) as u64
                );
            }
        }
    }
}

#[test]
#[cfg(all(target_arch = "x86_64", unix))]
fn generated_c_widening_matches_native_with_guarded_narrow_memory_sources() {
    if !std::arch::is_x86_feature_detected!("sse4.1") {
        return;
    }
    let mut bytes = Vec::new();
    for (_, opcode, _, _, _) in OPS {
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
    // Feature-test macros must precede every generated C system include.
    let mut c = String::from("#define _GNU_SOURCE\n");
    c.push_str(&emit::c(&p).unwrap());
    c.push_str(r#"
#include <emmintrin.h>
#include <assert.h>
#include <string.h>
#include <sys/mman.h>
#include <unistd.h>
typedef struct { uint8_t data[8]; unsigned width,loads; } memory;
static uint64_t load(void *ctx,uint64_t a,unsigned n) {
 memory *m=ctx;assert(a==128&&n==m->width&&m->loads++==0);
 uint64_t value=0;for(unsigned j=0;j<n;j++)value|=(uint64_t)m->data[j]<<(8*j);return value;
}
int main(void) {
 long page=sysconf(_SC_PAGESIZE);assert(page>0);
 uint8_t *guard=mmap(NULL,(size_t)page*2,PROT_READ|PROT_WRITE,MAP_PRIVATE|MAP_ANONYMOUS,-1,0);
 assert(guard!=MAP_FAILED&&mprotect(guard+page,(size_t)page,PROT_NONE)==0);
 const uint64_t boundary[]={0,UINT64_MAX,UINT64_C(0x8080808080808080),UINT64_C(0x7f7f7f7f7f7f7f7f),UINT64_C(0x8000800080008000),UINT64_C(0x7fff7fff7fff7fff),UINT64_C(0x8000000080000000),UINT64_C(0x7fffffff7fffffff)};
 uint64_t seed=73;
 for(unsigned trial=0;trial<65536;trial++) {
  uint64_t aa[2],bb[2];
  for(unsigned j=0;j<2;j++){seed=seed*UINT64_C(6364136223846793005)+1;aa[j]=seed;seed=seed*UINT64_C(6364136223846793005)+1;bb[j]=seed;if(trial<64){aa[j]=boundary[(trial/8+j)%8];bb[j]=boundary[(trial%8+3*j)%8];}}
"#);
    for (op_index, (_, _, _, _, width)) in OPS.into_iter().enumerate() {
        let size = width / 8;
        for i in &p.instructions[op_index * 6..op_index * 6 + 6] {
            let Operand::Vec(dst) = i.operands[0] else {
                unreachable!()
            };
            let alias = i.operands[0] == i.operands[1];
            let memory = matches!(i.operands[1], Operand::Mem { .. });
            writeln!(c,"{{memory mem={{.width={size}}};memcpy(mem.data,bb,{size});rosemary_machine m={{0}};for(unsigned j=0;j<16;j++){{m.r[j]=j*17;m.x[j]=(rosemary_vec){{UINT64_C(0x0123456789abcdef),UINT64_C(0xfedcba9876543210)}};}}m.x[0]=m.x[8]=(rosemary_vec){{aa[0],aa[1]}};m.x[1]=m.x[9]=(rosemary_vec){{bb[0],bb[1]}};m.r[3]=128;m.r[12]=80;m.r[13]=16;m.fs_base=m.gs_base=32;m.mxcsr=0x5fc0;m.ctx=&mem;m.load=load;m.rip={};m.cf=trial&1;m.pf=(trial>>1)&1;m.zf=(trial>>2)&1;m.sf=(trial>>3)&1;m.of=(trial>>4)&1;m.df=(trial>>5)&1;rosemary_machine before=m;assert(rosemary_run(&m,0)==1&&mem.loads==0&&m.rip==before.rip);assert(rosemary_run(&m,1)==1&&m.rip=={});__m128i expected;",i.address,i.address+u64::from(i.len)).unwrap();
            if alias {
                c.push_str("memcpy(&expected,aa,16);");
                writeln!(c, "__asm__ volatile(\"{} %0,%0\":\"+x\"(expected));", i.op).unwrap();
            } else if memory {
                writeln!(c,"memcpy(guard+page-{size},bb,{size});const uint8_t (*source)[{size}]=(const uint8_t (*)[{size}])(guard+page-{size});__asm__ volatile(\"{} %1,%0\":\"=x\"(expected):\"m\"(*source):\"memory\");",i.op).unwrap();
            } else {
                writeln!(c,"__m128i source;memcpy(&source,bb,16);__asm__ volatile(\"{} %1,%0\":\"=x\"(expected):\"x\"(source));",i.op).unwrap();
            }
            writeln!(c,"uint64_t words[2];memcpy(words,&expected,16);assert(m.x[{dst}].lo==words[0]&&m.x[{dst}].hi==words[1]);for(unsigned j=0;j<16;j++)if(j!={dst})assert(memcmp(&m.x[j],&before.x[j],sizeof(m.x[j]))==0);assert(mem.loads=={}&&memcmp(mem.data,bb,{size})==0&&memcmp(m.r,before.r,sizeof(m.r))==0&&m.mxcsr==before.mxcsr&&m.fs_base==before.fs_base&&m.gs_base==before.gs_base);assert(m.cf==before.cf&&m.pf==before.pf&&m.zf==before.zf&&m.sf==before.sf&&m.of==before.of&&m.df==before.df);}}",u8::from(memory)).unwrap();
        }
    }
    c.push_str("}assert(munmap(guard,(size_t)page*2)==0);return 0;}\n");
    let dir = std::env::temp_dir().join(format!("rosemary-widening-{}", std::process::id()));
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
        "status {:?}: {}",
        result.status,
        String::from_utf8_lossy(&result.stderr)
    );
    fs::remove_dir_all(dir).unwrap();
}
