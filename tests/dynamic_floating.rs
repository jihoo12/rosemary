use rosemary::{
    dynamic::{self, Flags, Machine, Options, Stop},
    emit,
    ir::{Instruction, Operand, Program, Segment},
};
use std::{fmt::Write, fs, process::Command};

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

fn flags(bits: usize) -> Flags {
    Flags {
        zf: bits & 1 != 0,
        sf: bits & 2 != 0,
        cf: bits & 4 != 0,
        of: bits & 8 != 0,
        pf: bits & 16 != 0,
        df: bits & 32 != 0,
    }
}

fn flag_bits(f: Flags) -> u8 {
    u8::from(f.zf)
        | (u8::from(f.sf) << 1)
        | (u8::from(f.cf) << 2)
        | (u8::from(f.of) << 3)
        | (u8::from(f.pf) << 4)
        | (u8::from(f.df) << 5)
}

#[test]
fn scalar_comparisons_match_c_for_nan_denormal_masks_and_daz() {
    let mut p = Program {
        entry: 0x1000,
        instructions: Vec::new(),
    };
    for op in ["comiss", "ucomiss", "comisd", "ucomisd"] {
        let width = if op.ends_with("ss") { 32 } else { 64 };
        let mut sources = vec![Operand::Vec(1), Operand::Vec(0)];
        for segment in [None, Some(Segment::Fs), Some(Segment::Gs)] {
            sources.push(Operand::Mem {
                segment,
                width,
                base: None,
                index: None,
                scale: 1,
                disp: if segment.is_some() { 0x2000 } else { 0x6000 },
            });
        }
        for source in sources {
            p.instructions.push(Instruction {
                address: 0x1000 + p.instructions.len() as u64,
                len: 1,
                op: op.into(),
                operands: vec![Operand::Vec(0), source],
            });
        }
    }
    let mut c = emit::c(&p).unwrap();
    c.push_str(r#"
#include <assert.h>
#include <inttypes.h>
#include <stdio.h>
#include <string.h>
typedef struct { uint64_t bits; unsigned loads; } memory;
static uint64_t load(void *ctx,uint64_t a,unsigned n) {
 memory *p=ctx;assert(a==0x6000&&(n==4||n==8));p->loads++;
 return n==4?(uint32_t)p->bits:p->bits;
}
int main(void) {
 uint64_t rip,a,b;unsigned csr,f;
 while(scanf("%" SCNx64 " %" SCNx64 " %" SCNx64 " %x %x",&rip,&a,&b,&csr,&f)==5) {
  rosemary_machine m={0};memory mem={b,0};
  m.rip=rip;m.mxcsr=csr;m.load=load;m.ctx=&mem;m.fs_base=m.gs_base=0x4000;
  for(unsigned j=0;j<16;j++){m.r[j]=j*17;m.x[j]=(rosemary_vec){UINT64_C(0x123456789abcdef),UINT64_C(0xfedcba9876543210)};}
  m.x[0].lo=a;m.x[1].lo=b;
  m.zf=f&1;m.sf=(f>>1)&1;m.cf=(f>>2)&1;m.of=(f>>3)&1;m.pf=(f>>4)&1;m.df=(f>>5)&1;
  rosemary_machine before=m;int status=rosemary_run(&m,1);
  assert(memcmp(m.r,before.r,sizeof(m.r))==0&&memcmp(m.x,before.x,sizeof(m.x))==0&&m.df==before.df);
  printf("%d %" PRIx64 " %x %x %u\n",status,m.rip,m.mxcsr,m.zf|(m.sf<<1)|(m.cf<<2)|(m.of<<3)|(m.pf<<4)|(m.df<<5),mem.loads);
 }
 return 0;
}
"#);
    let singles: [u64; 18] = [
        0, 0x80000000, 1, 0x80000001, 0x007fffff, 0x807fffff, 0x00800000, 0x80800000, 0x3f800000,
        0xbf800000, 0x7f7fffff, 0xff7fffff, 0x7f800000, 0xff800000, 0x7fc00000, 0xffc00042,
        0x7f800001, 0xff800001,
    ];
    let doubles: [u64; 18] = [
        0,
        0x8000000000000000,
        1,
        0x8000000000000001,
        0x000fffffffffffff,
        0x800fffffffffffff,
        0x0010000000000000,
        0x8010000000000000,
        0x3ff0000000000000,
        0xbff0000000000000,
        0x7fefffffffffffff,
        0xffefffffffffffff,
        0x7ff0000000000000,
        0xfff0000000000000,
        0x7ff8000000000000,
        0xfff8000000000042,
        0x7ff0000000000001,
        0xfff0000000000001,
    ];
    let mut input = String::new();
    let mut expected = String::new();
    let mut seed = 73u64;
    for i in &p.instructions {
        let values = if i.op.ends_with("ss") {
            &singles
        } else {
            &doubles
        };
        for environment in 0..20 {
            let csr = if environment < 16 {
                (0x1f80
                    & !(if environment & 1 != 0 { 0x80 } else { 0 }
                        | if environment & 2 != 0 { 0x100 } else { 0 }))
                    | if environment & 4 != 0 { 64 } else { 0 }
                    | if environment & 8 != 0 { 63 } else { 0 }
            } else {
                [0x1f80, 0x9fc0, 0x7f80, 0x3f80][environment - 16]
            };
            for trial in 0..388 {
                let (left, right) = if trial < 324 {
                    (values[trial / 18], values[trial % 18])
                } else {
                    seed = seed.wrapping_mul(6364136223846793005).wrapping_add(1);
                    let left = seed;
                    seed = seed.wrapping_mul(6364136223846793005).wrapping_add(1);
                    (left, seed)
                };
                let mut m = Machine::default();
                m.state.rip = i.address;
                m.state.mxcsr = csr;
                m.state.flags = flags(trial);
                m.state.fs_base = 0x4000;
                m.state.gs_base = 0x4000;
                for (n, r) in m.state.registers.iter_mut().enumerate() {
                    *r = n as u64 * 17;
                }
                m.state.xmm.fill(0xfedcba9876543210_0123456789abcdef);
                m.state.xmm[0] = (m.state.xmm[0] & (u128::MAX << 64)) | u128::from(left);
                m.state.xmm[1] = (m.state.xmm[1] & (u128::MAX << 64)) | u128::from(right);
                m.memory
                    .map(0x6000, right.to_le_bytes().to_vec(), false)
                    .unwrap();
                writeln!(
                    input,
                    "{:x} {left:x} {right:x} {csr:x} {:x}",
                    i.address,
                    trial & 63
                )
                .unwrap();
                let before = m.state;
                let report = run(&p, &mut m);
                assert_eq!(m.state.registers, before.registers);
                assert_eq!(m.state.xmm, before.xmm);
                assert_eq!(m.state.flags.df, before.flags.df);
                let trapped = matches!(report.stop, Stop::SimdException(_));
                assert!(trapped || matches!(report.stop, Stop::Budget | Stop::LeftCode));
                if trapped {
                    assert_eq!(m.state.rip, before.rip);
                    assert_eq!(m.state.flags, before.flags);
                    assert!(!report.trace[0].completed);
                }
                writeln!(
                    expected,
                    "{} {:x} {:x} {:x} {}",
                    if trapped { 2 } else { 1 },
                    m.state.rip,
                    m.state.mxcsr,
                    flag_bits(m.state.flags),
                    report.memory_reads
                )
                .unwrap();
            }
        }
    }
    let dir =
        std::env::temp_dir().join(format!("rosemary-dynamic-floating-{}", std::process::id()));
    fs::create_dir_all(&dir).unwrap();
    fs::write(dir.join("test.c"), c).unwrap();
    fs::write(dir.join("input"), input).unwrap();
    let result = Command::new("cc")
        .args(["-std=c11", "-O1", "-Wall", "-Wextra", "-Werror"])
        .arg(dir.join("test.c"))
        .arg("-o")
        .arg(dir.join("test"))
        .output()
        .unwrap();
    assert!(
        result.status.success(),
        "{}",
        String::from_utf8_lossy(&result.stderr)
    );
    let result = Command::new(dir.join("test"))
        .stdin(fs::File::open(dir.join("input")).unwrap())
        .output()
        .unwrap();
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
            "{:?}, environment {}, trial {}",
            p.instructions[n / (20 * 388)],
            n / 388 % 20,
            n % 388
        );
    }
    fs::remove_dir_all(dir).unwrap();
}

#[test]
fn simd_exceptions_keep_status_and_resume_after_masking() {
    for (value, exception, label) in [(0x7f800001, 1, "invalid"), (1, 2, "denormal")] {
        let p = Program::from_text(
            "rosemary-ir 1\nentry 1000\n1000 1 ucomiss x:128:0 m:32:-:-:1:2000\n",
        )
        .unwrap();
        let mut m = Machine::with_stack(p.entry);
        assert_eq!(m.state.mxcsr, 0x1f80);
        m.state.mxcsr &= !(exception << 7);
        m.state.xmm[0] = value;
        m.state.flags = flags(63);
        let before = m.state;
        // A memory fault takes priority and cannot raise a SIMD exception.
        assert!(matches!(run(&p, &mut m).stop, Stop::MemoryFault(_)));
        assert_eq!(m.state, before);
        m.memory.map(0x2000, vec![0; 4], false).unwrap();
        let r = run(&p, &mut m);
        assert_eq!(r.stop, Stop::SimdException(exception as u8));
        assert_eq!(r.stop.name(), "simd-exception");
        assert!(r.stop.detail().contains(label));
        let mut expected = before;
        expected.mxcsr |= exception;
        assert_eq!(m.state, expected);
        assert_eq!(r.steps, 1);
        assert!(r.edges.is_empty());
        assert_eq!(r.memory_reads, 1);
        assert!(r.text().contains("mxcsr:"));
        assert!(r.json().contains("\"stop\":\"simd-exception\""));
        m.state.mxcsr |= exception << 7;
        assert_eq!(run(&p, &mut m).stop, Stop::LeftCode);
        assert_eq!(m.state.rip, 0x1001);
        assert_eq!(m.state.mxcsr & exception, exception);
        assert_eq!(m.state.xmm, before.xmm);
    }
}
