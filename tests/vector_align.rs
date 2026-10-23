use rosemary::{
    dataflow::{self, Flags, State},
    decode,
    dynamic::{self, Machine, Options, Stop},
    emit,
    ir::{Operand, Program, Segment},
};
#[cfg(target_arch = "x86_64")]
use std::{fmt::Write, fs, process::Command};

#[test]
fn align_decodes_all_registers_immediates_and_complete_rip_relative_lengths() {
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
    for dst in 0..16 {
        for src in 0..16 {
            for w in [0, 8] {
                let count = (dst << 4) | src;
                let bytes = [
                    0x66,
                    0x40 | w | ((dst >> 3) << 2) | (src >> 3),
                    0x0f,
                    0x3a,
                    0x0f,
                    0xc0 | ((dst & 7) << 3) | (src & 7),
                    count,
                ];
                let p = decode::decode(&bytes, 0x1000, None).unwrap();
                let i = &p.instructions[0];
                assert_eq!(i.op, "palignr");
                assert_eq!(i.len as usize, bytes.len());
                assert_eq!(
                    i.operands,
                    [
                        Operand::Vec(dst),
                        Operand::Vec(src),
                        Operand::Imm(count.into(), 8)
                    ]
                );
                assert_eq!(Program::from_text(&p.to_text().unwrap()).unwrap(), p);
                assert_eq!(dataflow::transfer(i, state), state);
            }
        }
    }
    for (prefix, segment) in [(0x64, Segment::Fs), (0x65, Segment::Gs)] {
        let p = decode::decode(
            &[prefix, 0x66, 0x47, 0x0f, 0x3a, 0x0f, 0x44, 0x6c, 0xf0, 8],
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
                    disp: (-16i64) as u64,
                },
                Operand::Imm(8, 8)
            ]
        );
        assert!(
            emit::assembly(&p)
                .unwrap()
                .contains(&format!("xmmword ptr {}:", segment.name()))
        );
    }
    let bytes = [
        0x66, 0x44, 0x0f, 0x3a, 0x0f, 0x05, 0xf0, 0xff, 0xff, 0xff, 0xff,
    ];
    let p = decode::decode(&bytes, 0x1000, None).unwrap();
    assert_eq!(
        p.instructions[0].operands,
        [
            Operand::Vec(8),
            Operand::Mem {
                segment: None,
                width: 128,
                base: None,
                index: None,
                scale: 1,
                disp: 0x1000 + bytes.len() as u64 - 16,
            },
            Operand::Imm(255, 8)
        ]
    );
    for size in 0..bytes.len() {
        assert!(
            decode::decode(&bytes[..size], 0x1000, None).is_err(),
            "length {size}"
        );
    }
}

#[test]
fn align_rejects_mmx_wrong_prefixes_and_malformed_ir() {
    for prefix in [
        vec![],
        vec![0xf2],
        vec![0xf3],
        vec![0x66, 0xf3],
        vec![0xf0, 0x66],
    ] {
        let mut bytes = prefix;
        bytes.extend([0x0f, 0x3a, 0x0f, 0xc1, 8]);
        assert!(decode::decode(&bytes, 0x1000, None).is_err());
    }
    assert!(decode::decode(&[0x66, 0x0f, 0x3a, 0x00, 0xc1, 8], 0x1000, None).is_err());
    let mut overlong = vec![0x66; 14];
    overlong.extend([0x0f, 0x3a, 0x0f, 0xc1, 8]);
    assert!(decode::decode(&overlong, 0x1000, None).is_err());
    for operands in [
        "x:128:0 x:128:1",
        "x:128:0 x:128:1 i:32:8",
        "x:128:0 x:128:1 i:8:100",
        "x:128:0 r:64:0 i:8:0",
        "x:128:0 m:64:0:-:1:0 i:8:0",
        "m:128:0:-:1:0 x:128:0 i:8:0",
        "x:128:0 x:128:1 r:8:1",
    ] {
        assert!(
            Program::from_text(&format!(
                "rosemary-ir 1\nentry 1000\n1000 1 palignr {operands}\n"
            ))
            .is_err()
        );
    }
}

#[test]
fn mixed_products_and_align_read_full_memory_even_for_zero_results() {
    for op in ["pmaddubsw", "palignr"] {
        for count in [0, 16, 32, 255] {
            let immediate = if op == "palignr" {
                format!(" i:8:{count:x}")
            } else {
                String::new()
            };
            let p = Program::from_text(&format!(
                "rosemary-ir 1\nentry 1000\n1000 1 {op} x:128:15 m:128:-:-:1:2000{immediate}\n"
            ))
            .unwrap();
            let mut m = Machine::with_stack(p.entry);
            m.state.xmm[15] = 0x0f0e_0d0c_0b0a_0908_0706_0504_0302_0100;
            m.state.flags.cf = true;
            m.state.flags.df = true;
            m.memory.map(0x2000, vec![0x81; 8], false).unwrap();
            let before = m.state;
            let r = dynamic::execute(&p, &mut m, Options::default()).unwrap();
            assert!(matches!(r.stop, Stop::MemoryFault(_)));
            assert_eq!(m.state, before);
            assert_eq!((r.memory_reads, r.memory_writes), (1, 0));
            assert!(!r.trace[0].completed);
            assert!(r.edges.is_empty());
            m.memory.map(0x2008, vec![0x7f; 8], false).unwrap();
            let r = dynamic::execute(&p, &mut m, Options::default()).unwrap();
            assert_eq!(r.stop, Stop::LeftCode);
            assert_eq!((r.memory_reads, r.memory_writes), (2, 0));
            assert_eq!(m.state.flags, before.flags);
            assert_eq!(m.state.registers, before.registers);
            if op == "palignr" {
                assert_eq!(
                    m.state.xmm[15],
                    match count {
                        0 => 0x7f7f_7f7f_7f7f_7f7f_8181_8181_8181_8181,
                        16 => before.xmm[15],
                        _ => 0,
                    }
                );
            }
        }
    }
}

#[test]
#[cfg(target_arch = "x86_64")]
fn align_c_and_concrete_execution_match_native_for_all_immediate_bytes() {
    if !std::arch::is_x86_feature_detected!("ssse3") {
        return;
    }
    let mut bytes = Vec::new();
    for count in 0..=255 {
        bytes.extend([0x66, 0x0f, 0x3a, 0x0f, 0xc1, count]);
    }
    for count in [0, 1, 7, 8, 9, 15, 16, 17, 23, 24, 25, 31, 32, 33, 255] {
        for (prefix, suffix) in [
            (&[0x66][..], &[0xc0][..]),
            (&[0x66, 0x45][..], &[0xc1][..]),
            (&[0x66][..], &[0x03][..]),
            (&[0x64, 0x66, 0x47][..], &[0x44, 0x6c, 0xf0][..]),
            (&[0x65, 0x66, 0x47][..], &[0x44, 0x6c, 0xf0][..]),
        ] {
            bytes.extend(prefix);
            bytes.extend([0x0f, 0x3a, 0x0f]);
            bytes.extend(suffix);
            bytes.push(count);
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
static uint64_t load(void *ctx,uint64_t address,unsigned width){memory *mem=ctx;assert(width==8&&mem->loads<2&&address==128+8*mem->loads);return mem->words[mem->loads++];}
static const uint64_t trials[64][4]={
"#);
    let mut trials = Vec::new();
    let mut seed = 7u64;
    for trial in 0..64 {
        let mut words = [0; 4];
        for word in &mut words {
            seed = seed.wrapping_mul(6364136223846793005).wrapping_add(1);
            *word = seed;
        }
        if trial == 0 {
            words = [
                0x0706050403020100,
                0x0f0e0d0c0b0a0908,
                0x1716151413121110,
                0x1f1e1d1c1b1a1918,
            ];
        }
        if trial == 1 {
            words = [u64::MAX; 4];
        }
        if trial == 2 {
            words = [0; 4];
        }
        writeln!(
            c,
            "{{{}}},",
            words
                .iter()
                .map(|v| format!("UINT64_C(0x{v:x})"))
                .collect::<Vec<_>>()
                .join(",")
        )
        .unwrap();
        trials.push(words);
    }
    c.push_str("};\nint main(void){for(unsigned trial=0;trial<64;trial++){_Alignas(16) uint64_t aa[2]={trials[trial][0],trials[trial][1]},bb[2]={trials[trial][2],trials[trial][3]};\n");
    for i in &p.instructions {
        let Operand::Vec(dst) = i.operands[0] else {
            unreachable!()
        };
        let Operand::Imm(count, _) = i.operands[2] else {
            unreachable!()
        };
        let alias = i.operands[0] == i.operands[1];
        let memory = matches!(i.operands[1], Operand::Mem { .. });
        writeln!(c,"{{memory mem={{{{bb[0],bb[1]}},0}};rosemary_machine m={{0}};for(unsigned j=0;j<16;j++){{m.r[j]=j*17;m.x[j]=(rosemary_vec){{UINT64_C(0x0123456789abcdef),UINT64_C(0xfedcba9876543210)}};}}m.x[0]=m.x[8]=(rosemary_vec){{aa[0],aa[1]}};m.x[1]=m.x[9]=(rosemary_vec){{bb[0],bb[1]}};m.r[3]=128;m.r[12]=80;m.r[13]=16;m.fs_base=m.gs_base=32;m.mxcsr=0x5fc0;m.ctx=&mem;m.load=load;m.rip={};m.cf=trial&1;m.pf=(trial>>1)&1;m.zf=(trial>>2)&1;m.sf=(trial>>3)&1;m.of=(trial>>4)&1;m.df=(trial>>5)&1;rosemary_machine before=m;__m128i expected;memcpy(&expected,aa,16);assert(rosemary_run(&m,1)==1);",i.address).unwrap();
        if alias {
            writeln!(
                c,
                "__asm__ volatile(\"palignr ${count},%0,%0\":\"+x\"(expected));"
            )
            .unwrap();
        } else if memory {
            writeln!(c,"__asm__ volatile(\"palignr ${count},%1,%0\":\"+x\"(expected):\"m\"(bb):\"memory\");").unwrap();
        } else {
            writeln!(c,"__m128i source;memcpy(&source,bb,16);__asm__ volatile(\"palignr ${count},%1,%0\":\"+x\"(expected):\"x\"(source));").unwrap();
        }
        writeln!(c,"uint64_t words[2];memcpy(words,&expected,16);assert(m.x[{dst}].lo==words[0]&&m.x[{dst}].hi==words[1]);for(unsigned j=0;j<16;j++)if(j!={dst})assert(memcmp(&m.x[j],&before.x[j],sizeof(m.x[j]))==0);assert(mem.loads=={}&&memcmp(mem.words,bb,16)==0&&memcmp(m.r,before.r,sizeof(m.r))==0&&m.mxcsr==before.mxcsr&&m.fs_base==before.fs_base&&m.gs_base==before.gs_base);assert(m.cf==before.cf&&m.pf==before.pf&&m.zf==before.zf&&m.sf==before.sf&&m.of==before.of&&m.df==before.df);printf(\"%\" PRIx64 \" %\" PRIx64 \"\\n\",words[0],words[1]);}}",if memory {2} else {0}).unwrap();
    }
    c.push_str("}return 0;}\n");
    let mut expected = String::new();
    for (trial, words) in trials.iter().enumerate() {
        for i in &p.instructions {
            let mut m = Machine::default();
            m.state.rip = i.address;
            m.state.mxcsr = 0x5fc0;
            m.state.fs_base = 32;
            m.state.gs_base = 32;
            m.state.xmm.fill(0xfedcba9876543210_0123456789abcdef);
            for (n, r) in m.state.registers.iter_mut().enumerate() {
                *r = n as u64 * 17;
            }
            m.state.registers[3] = 128;
            m.state.registers[12] = 80;
            m.state.registers[13] = 16;
            m.state.xmm[0] = u128::from(words[0]) | (u128::from(words[1]) << 64);
            m.state.xmm[8] = m.state.xmm[0];
            m.state.xmm[1] = u128::from(words[2]) | (u128::from(words[3]) << 64);
            m.state.xmm[9] = m.state.xmm[1];
            m.state.flags.cf = trial & 1 != 0;
            m.state.flags.pf = trial & 2 != 0;
            m.state.flags.zf = trial & 4 != 0;
            m.state.flags.sf = trial & 8 != 0;
            m.state.flags.of = trial & 16 != 0;
            m.state.flags.df = trial & 32 != 0;
            m.memory
                .map(
                    128,
                    [words[2].to_le_bytes(), words[3].to_le_bytes()].concat(),
                    false,
                )
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
            assert_eq!(report.memory_writes, 0);
            assert_eq!(
                report.memory_reads,
                if matches!(i.operands[1], Operand::Mem { .. }) {
                    2
                } else {
                    0
                }
            );
            writeln!(expected, "{:x} {:x}", value as u64, value >> 64).unwrap();
        }
    }
    let dir = std::env::temp_dir().join(format!("rosemary-align-{}", std::process::id()));
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
