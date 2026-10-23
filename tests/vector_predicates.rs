use rosemary::{
    dataflow::{self, Flags, State},
    decode,
    dynamic::{self, Machine, Options, Stop},
    emit,
    ir::{Operand, Program, Segment},
};
#[cfg(target_arch = "x86_64")]
use std::{fmt::Write, fs, process::Command};

const OPS: [(&str, u8); 1] = [("pcmpeqq", 0x29)];
const FORMS: [(&[u8], &[u8]); 6] = [
    (&[0x66], &[0xc1]),
    (&[0x66], &[0xc0]),
    (&[0x66, 0x45], &[0xc1]),
    (&[0x66], &[0x03]),
    (&[0x64, 0x66, 0x47], &[0x44, 0x6c, 0xf0]),
    (&[0x65, 0x66, 0x47], &[0x44, 0x6c, 0xf0]),
];

fn cases(opcode: u8) -> Program {
    let mut bytes = Vec::new();
    for (prefix, suffix) in FORMS {
        bytes.extend(prefix);
        bytes.extend([0x0f, 0x38, opcode]);
        bytes.extend(suffix);
    }
    decode::decode(&bytes, 0x1000, None).unwrap()
}

fn machine(rip: u64, left: u128, right: u128, flags: usize) -> Machine {
    let mut m = Machine::default();
    m.state.rip = rip;
    m.state.fs_base = 32;
    m.state.gs_base = 32;
    m.state.mxcsr = 0x5fc0;
    for (n, r) in m.state.registers.iter_mut().enumerate() {
        *r = n as u64 * 17;
    }
    m.state.registers[3] = 128;
    m.state.registers[12] = 80;
    m.state.registers[13] = 16;
    m.state.xmm.fill(0xfedcba9876543210_0123456789abcdef);
    m.state.xmm[0] = left;
    m.state.xmm[8] = left;
    m.state.xmm[1] = right;
    m.state.xmm[9] = right;
    m.state.flags.cf = flags & 1 != 0;
    m.state.flags.pf = flags & 2 != 0;
    m.state.flags.zf = flags & 4 != 0;
    m.state.flags.sf = flags & 8 != 0;
    m.state.flags.of = flags & 16 != 0;
    m.state.flags.df = flags & 32 != 0;
    m.memory
        .map(128, right.to_le_bytes().to_vec(), false)
        .unwrap();
    m
}

fn run(p: &Program, m: &mut Machine) -> dynamic::Report {
    dynamic::execute(
        p,
        m,
        Options {
            max_steps: 1,
            trace_limit: 1,
        },
    )
    .unwrap()
}

#[test]
fn predicate_encodings_retain_registers_segments_and_reject_invalid_forms() {
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
fn equality_uses_both_dwords_of_each_qword_and_preserves_scalar_state() {
    let p = decode::decode(&[0x66, 0x0f, 0x38, 0x29, 0xc1], 0x1000, None).unwrap();
    let left = 0x0123456789abcdef_fedcba9876543210;
    for bit in 0..128 {
        let mut m = machine(p.entry, left, left ^ (1u128 << bit), 63);
        let before = m.state;
        assert_eq!(run(&p, &mut m).stop, Stop::LeftCode);
        let mut expected = before;
        expected.rip += u64::from(p.instructions[0].len);
        expected.xmm[0] = if bit < 64 {
            u128::MAX << 64
        } else {
            u128::from(u64::MAX)
        };
        assert_eq!(m.state, expected, "mismatching bit {bit}");
    }
}

#[test]
fn predicates_fault_before_changing_vectors_or_flags_and_can_resume() {
    for (_, opcode) in OPS {
        let p = decode::decode(&[0x66, 0x44, 0x0f, 0x38, opcode, 0x03], 0x1000, None).unwrap();
        let mut m = Machine::with_stack(p.entry);
        m.state.registers[3] = 0x2000;
        m.state.xmm[8] = u128::MAX;
        m.state.flags.cf = true;
        m.state.flags.of = true;
        m.state.flags.pf = true;
        m.state.flags.df = true;
        m.memory.map(0x2000, vec![255; 8], false).unwrap();
        let before = m.state;
        let report = run(&p, &mut m);
        assert!(matches!(report.stop, Stop::MemoryFault(_)));
        assert_eq!(m.state, before);
        assert_eq!((report.memory_reads, report.memory_writes), (1, 0));
        assert!(!report.trace[0].completed);
        m.memory.map(0x2008, vec![255; 8], false).unwrap();
        let report = run(&p, &mut m);
        assert_eq!(report.stop, Stop::LeftCode);
        assert_eq!((report.memory_reads, report.memory_writes), (2, 0));
        let mut expected = before;
        expected.rip += u64::from(p.instructions[0].len);
        assert_eq!(m.state, expected);
    }
}

#[cfg(target_arch = "x86_64")]
fn inputs(trial: usize, seed: &mut u64) -> (u128, u128) {
    let boundary = [
        0,
        1,
        u64::MAX,
        1 << 63,
        (1 << 63) - 1,
        0xffff_ffff,
        1 << 32,
        0xffff_ffff_0000_0000,
    ];
    let mut left = 0;
    let mut right = 0;
    for lane in 0..2 {
        *seed = seed.wrapping_mul(6364136223846793005).wrapping_add(1);
        let mut a = *seed;
        *seed = seed.wrapping_mul(6364136223846793005).wrapping_add(1);
        let mut b = *seed;
        if trial < 64 {
            a = boundary[(trial / 8 + lane) % 8];
            b = boundary[(trial % 8 + 3 * lane) % 8];
        }
        left |= u128::from(a) << (64 * lane);
        right |= u128::from(b) << (64 * lane);
    }
    if trial >= 64 {
        right = match trial & 7 {
            1 => left,
            2 => left ^ (1u128 << ((trial / 8) % 128)),
            3 => left ^ (1u128 << 64),
            4 => !left,
            5 => left & right,
            6 => !left & right,
            7 => 0,
            _ => right,
        };
    }
    (left, right)
}

#[cfg(target_arch = "x86_64")]
#[target_feature(enable = "sse4.1")]
unsafe fn native_equal(left: u128, right: u128) -> u128 {
    use std::arch::x86_64::{__m128i, _mm_cmpeq_epi64};
    let left = unsafe { std::mem::transmute::<u128, __m128i>(left) };
    let right = unsafe { std::mem::transmute::<u128, __m128i>(right) };
    unsafe { std::mem::transmute::<__m128i, u128>(_mm_cmpeq_epi64(left, right)) }
}

#[test]
#[cfg(target_arch = "x86_64")]
fn concrete_qword_equality_matches_native_across_aliases_and_full_width_inputs() {
    if !std::arch::is_x86_feature_detected!("sse4.1") {
        return;
    }
    let p = cases(0x29);
    let mut seed = 73;
    for trial in 0..65536 {
        let (left, right) = inputs(trial, &mut seed);
        for i in &p.instructions {
            let alias = i.operands[0] == i.operands[1];
            let memory = matches!(i.operands[1], Operand::Mem { .. });
            let mut m = machine(i.address, left, right, trial);
            let before = m.state;
            // The runtime SSE4.1 check above protects the native instruction.
            let value = unsafe { native_equal(left, if alias { left } else { right }) };
            let report = run(&p, &mut m);
            assert!(matches!(report.stop, Stop::Budget | Stop::LeftCode));
            let Operand::Vec(dst) = i.operands[0] else {
                unreachable!()
            };
            let mut expected = before;
            expected.rip += u64::from(i.len);
            expected.xmm[dst as usize] = value;
            assert_eq!(m.state, expected, "trial {trial}, {}", i.op);
            assert_eq!(
                (report.memory_reads, report.memory_writes),
                (if memory { 2 } else { 0 }, 0)
            );
        }
    }
}

#[test]
#[cfg(target_arch = "x86_64")]
fn c_qword_equality_matches_native_and_generated_assembly_is_valid() {
    if !std::arch::is_x86_feature_detected!("sse4.1") {
        return;
    }
    let p = cases(0x29);
    let mut c = emit::c(&p).unwrap();
    c.push_str(r#"
#include <emmintrin.h>
#include <assert.h>
#include <string.h>
typedef struct{uint64_t words[2];unsigned loads;} memory;
static uint64_t load(void *ctx,uint64_t a,unsigned n){memory *m=ctx;assert(n==8&&m->loads<2&&a==128+8*m->loads);return m->words[m->loads++];}
int main(void){uint64_t seed=73;for(unsigned trial=0;trial<65536;trial++){
 _Alignas(16) uint64_t aa[2],bb[2];
 for(unsigned j=0;j<2;j++){seed=seed*UINT64_C(6364136223846793005)+1;aa[j]=seed;seed=seed*UINT64_C(6364136223846793005)+1;bb[j]=seed;}
 switch(trial&7){case 1:bb[0]=aa[0];bb[1]=aa[1];break;case 2:bb[0]=aa[0]^1;bb[1]=aa[1];break;case 3:bb[0]=aa[0];bb[1]=aa[1]^(UINT64_C(1)<<32);break;case 4:bb[0]=~aa[0];bb[1]=~aa[1];break;case 5:bb[0]&=aa[0];bb[1]&=aa[1];break;case 6:bb[0]&=~aa[0];bb[1]&=~aa[1];break;case 7:bb[0]=bb[1]=0;break;default:break;}
"#);
    for i in &p.instructions {
        let Operand::Vec(dst) = i.operands[0] else {
            unreachable!()
        };
        let alias = i.operands[0] == i.operands[1];
        let memory = matches!(i.operands[1], Operand::Mem { .. });
        writeln!(c,"{{memory mem={{{{bb[0],bb[1]}},0}};rosemary_machine m={{0}};for(unsigned j=0;j<16;j++){{m.r[j]=j*17;m.x[j]=(rosemary_vec){{UINT64_C(0x0123456789abcdef),UINT64_C(0xfedcba9876543210)}};}}m.x[0]=m.x[8]=(rosemary_vec){{aa[0],aa[1]}};m.x[1]=m.x[9]=(rosemary_vec){{bb[0],bb[1]}};m.r[3]=128;m.r[12]=80;m.r[13]=16;m.fs_base=m.gs_base=32;m.mxcsr=0x5fc0;m.ctx=&mem;m.load=load;m.rip={};m.cf=trial&1;m.pf=(trial>>1)&1;m.zf=(trial>>2)&1;m.sf=(trial>>3)&1;m.of=(trial>>4)&1;m.df=(trial>>5)&1;rosemary_machine before=m;assert(rosemary_run(&m,1)==1&&m.rip=={});__m128i expected;memcpy(&expected,aa,16);",i.address,i.address+u64::from(i.len)).unwrap();
        if alias {
            c.push_str("__asm__ volatile(\"pcmpeqq %0,%0\":\"+x\"(expected));\n");
        } else if memory {
            c.push_str(
                "__asm__ volatile(\"pcmpeqq %1,%0\":\"+x\"(expected):\"m\"(bb):\"memory\");\n",
            );
        } else {
            c.push_str("__m128i source;memcpy(&source,bb,16);__asm__ volatile(\"pcmpeqq %1,%0\":\"+x\"(expected):\"x\"(source));\n");
        }
        writeln!(c,"uint64_t words[2];memcpy(words,&expected,16);assert(m.x[{dst}].lo==words[0]&&m.x[{dst}].hi==words[1]);for(unsigned j=0;j<16;j++)if(j!={dst})assert(memcmp(&m.x[j],&before.x[j],sizeof(m.x[j]))==0);assert(mem.loads=={}&&memcmp(mem.words,bb,16)==0&&memcmp(m.r,before.r,sizeof(m.r))==0&&m.mxcsr==before.mxcsr&&m.fs_base==before.fs_base&&m.gs_base==before.gs_base);assert(m.cf==before.cf&&m.pf==before.pf&&m.zf==before.zf&&m.sf==before.sf&&m.of==before.of&&m.df==before.df);}}",if memory{2}else{0}).unwrap();
    }
    c.push_str("}return 0;}\n");
    compile("equality", &p, &c);
}

#[cfg(target_arch = "x86_64")]
fn compile(name: &str, p: &Program, c: &str) {
    let dir =
        std::env::temp_dir().join(format!("rosemary-predicates-{name}-{}", std::process::id()));
    fs::create_dir_all(&dir).unwrap();
    fs::write(dir.join("test.c"), c).unwrap();
    fs::write(dir.join("test.s"), emit::assembly(p).unwrap()).unwrap();
    for (source, output, flags) in [
        (
            "test.c",
            "test",
            vec![
                "-std=c11",
                "-O1",
                "-msse2",
                "-mno-red-zone",
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
