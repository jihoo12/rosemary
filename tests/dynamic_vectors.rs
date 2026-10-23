use rosemary::{
    dynamic::{self, Flags, Machine, Options, Stop},
    emit,
    ir::{Instruction, Operand, Program},
};
use std::{fmt::Write, fs, process::Command};

fn execute(p: &Program, m: &mut Machine) -> dynamic::Report {
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

fn vector_program() -> Program {
    let mut p = Program {
        entry: 0x1000,
        instructions: Vec::new(),
    };
    let mut add = |op: &str, operands: Vec<Operand>| {
        p.instructions.push(Instruction {
            address: 0x1000 + p.instructions.len() as u64,
            len: 1,
            op: op.into(),
            operands,
        });
    };
    let memory = |width| Operand::Mem {
        segment: None,
        width,
        base: Some(3),
        index: None,
        scale: 1,
        disp: 0,
    };
    for op in [
        "movups", "movaps", "movupd", "movapd", "movdqu", "movdqa", "movsldup", "movshdup", "pxor",
        "xorps", "xorpd", "pand", "andps", "andpd", "pandn", "andnps", "andnpd", "por", "orps",
        "orpd",
    ] {
        for source in [Operand::Vec(1), Operand::Vec(0), memory(128)] {
            add(op, vec![Operand::Vec(0), source]);
        }
        if matches!(
            op,
            "movups" | "movaps" | "movupd" | "movapd" | "movdqu" | "movdqa"
        ) {
            add(op, vec![memory(128), Operand::Vec(0)]);
        }
    }
    for (op, bits) in [("movd", 32), ("movq", 64), ("movss", 32), ("movsd", 64)] {
        add(op, vec![Operand::Vec(0), memory(bits)]);
        add(op, vec![memory(bits), Operand::Vec(0)]);
        if matches!(op, "movd" | "movq") {
            add(op, vec![Operand::Vec(0), Operand::Reg(0, bits)]);
            add(op, vec![Operand::Reg(0, bits), Operand::Vec(0)]);
        }
        if op != "movd" {
            for r in [0, 1] {
                add(op, vec![Operand::Vec(0), Operand::Vec(r)]);
            }
        }
    }
    for op in ["movlps", "movhps", "movlpd", "movhpd"] {
        add(op, vec![Operand::Vec(0), memory(64)]);
        add(op, vec![memory(64), Operand::Vec(0)]);
    }
    for source in [Operand::Vec(0), Operand::Vec(1), memory(64)] {
        add("movddup", vec![Operand::Vec(0), source]);
    }
    for (op, width) in [
        ("pmovsxbw", 64),
        ("pmovsxbd", 32),
        ("pmovsxbq", 16),
        ("pmovsxwd", 64),
        ("pmovsxwq", 32),
        ("pmovsxdq", 64),
        ("pmovzxbw", 64),
        ("pmovzxbd", 32),
        ("pmovzxbq", 16),
        ("pmovzxwd", 64),
        ("pmovzxwq", 32),
        ("pmovzxdq", 64),
    ] {
        for source in [Operand::Vec(1), Operand::Vec(0), memory(width)] {
            add(op, vec![Operand::Vec(0), source]);
        }
    }
    for op in ["movlhps", "movhlps"] {
        for r in [0, 1] {
            add(op, vec![Operand::Vec(0), Operand::Vec(r)]);
        }
    }
    for op in ["pmovmskb", "movmskps", "movmskpd"] {
        add(op, vec![Operand::Reg(0, 32), Operand::Vec(0)]);
    }
    for index in [0, 1, 7, 8, 255] {
        add(
            "pextrw",
            vec![Operand::Reg(0, 32), Operand::Vec(0), Operand::Imm(index, 8)],
        );
        for source in [Operand::Reg(0, 32), memory(16)] {
            add(
                "pinsrw",
                vec![Operand::Vec(0), source, Operand::Imm(index, 8)],
            );
        }
    }
    for op in [
        "paddb",
        "phaddw",
        "phaddd",
        "phaddsw",
        "phsubw",
        "phsubd",
        "phsubsw",
        "pabsb",
        "pabsw",
        "pabsd",
        "psignb",
        "psignw",
        "psignd",
        "paddw",
        "paddd",
        "paddq",
        "psubb",
        "psubw",
        "psubd",
        "psubq",
        "paddsb",
        "paddsw",
        "psubsb",
        "psubsw",
        "paddusb",
        "paddusw",
        "psubusb",
        "psubusw",
        "pavgb",
        "pavgw",
        "pcmpeqb",
        "pcmpeqw",
        "pcmpeqd",
        "pcmpeqq",
        "ptest",
        "pcmpgtb",
        "pcmpgtw",
        "pcmpgtd",
        "pminub",
        "pminsb",
        "pminsd",
        "pminuw",
        "pminud",
        "pmaxub",
        "pmaxsb",
        "pmaxsd",
        "pmaxuw",
        "pmaxud",
        "pminsw",
        "pmaxsw",
        "pmullw",
        "pmulld",
        "pmulhw",
        "pmulhuw",
        "pmulhrsw",
        "pmuludq",
        "pmuldq",
        "pmaddwd",
        "pmaddubsw",
        "psadbw",
        "pshufb",
        "packsswb",
        "packssdw",
        "packuswb",
        "punpcklbw",
        "punpcklwd",
        "punpckldq",
        "punpcklqdq",
        "punpckhbw",
        "punpckhwd",
        "punpckhdq",
        "punpckhqdq",
    ] {
        for source in [Operand::Vec(1), Operand::Vec(0), memory(128)] {
            add(op, vec![Operand::Vec(0), source]);
        }
    }
    for op in [
        "psllw", "pslld", "psllq", "psrlw", "psrld", "psrlq", "psraw", "psrad",
    ] {
        for source in [Operand::Vec(1), Operand::Vec(0), memory(128)] {
            add(op, vec![Operand::Vec(0), source]);
        }
        for count in [0, 1, 7, 8, 15, 16, 17, 31, 32, 33, 63, 64, 65, 128, 255] {
            add(op, vec![Operand::Vec(0), Operand::Imm(count, 8)]);
        }
    }
    for op in ["pslldq", "psrldq"] {
        for count in [0, 1, 7, 8, 9, 15, 16, 17, 255] {
            add(op, vec![Operand::Vec(0), Operand::Imm(count, 8)]);
        }
    }
    for op in ["pshufd", "pshuflw", "pshufhw", "shufps", "shufpd"] {
        for source in [Operand::Vec(1), Operand::Vec(0), memory(128)] {
            for select in [0, 1, 3, 4, 0x1b, 0x55, 0xaa, 0xe4, 0xff] {
                add(
                    op,
                    vec![Operand::Vec(0), source.clone(), Operand::Imm(select, 8)],
                );
            }
        }
    }
    for source in [Operand::Vec(1), Operand::Vec(0), memory(128)] {
        for count in [0, 1, 7, 8, 9, 15, 16, 17, 23, 24, 25, 31, 32, 33, 255] {
            add(
                "palignr",
                vec![Operand::Vec(0), source.clone(), Operand::Imm(count, 8)],
            );
        }
    }
    p.validate().unwrap();
    p
}

fn snapshot(out: &mut String, m: &Machine) {
    write!(out, "{:x}", m.state.rip).unwrap();
    for r in m.state.registers {
        write!(out, " {r:x}").unwrap();
    }
    for v in m.state.xmm {
        write!(out, " {:x} {:x}", v as u64, v >> 64).unwrap();
    }
    let f = m.state.flags;
    let flags = u8::from(f.zf)
        | (u8::from(f.sf) << 1)
        | (u8::from(f.cf) << 2)
        | (u8::from(f.of) << 3)
        | (u8::from(f.pf) << 4)
        | (u8::from(f.df) << 5);
    write!(out, " {flags:x} ").unwrap();
    for n in 0..64 {
        write!(out, "{:02x}", m.memory.load(0x2000 + n, 1).unwrap()).unwrap();
    }
    out.push('\n');
}

#[test]
fn concrete_vectors_match_generated_c_with_aliases_memory_and_boundary_bits() {
    let p = vector_program();
    let mut c = emit::c(&p).unwrap();
    c.push_str(
        r#"
#include <assert.h>
#include <inttypes.h>
#include <stdio.h>
static uint64_t load(void *ctx,uint64_t a,unsigned n) {
 assert(a>=0x2000 && a+n<=0x2040); uint8_t *p=ctx;uint64_t v=0;
 for(unsigned j=0;j<n;j++)v|=(uint64_t)p[a-0x2000+j]<<(8*j);return v;
}
static void store(void *ctx,uint64_t a,unsigned n,uint64_t v) {
 assert(a>=0x2000 && a+n<=0x2040); uint8_t *p=ctx;
 for(unsigned j=0;j<n;j++)p[a-0x2000+j]=(uint8_t)(v>>(8*j));
}
int main(void) {
 uint64_t rip;
 while(scanf("%" SCNx64,&rip)==1) {
  rosemary_machine m={0};uint8_t memory[64];unsigned f;
  m.rip=rip;m.load=load;m.store=store;m.ctx=memory;
  for(unsigned j=0;j<16;j++)assert(scanf("%" SCNx64,&m.r[j])==1);
  for(unsigned j=0;j<16;j++)assert(scanf("%" SCNx64 " %" SCNx64,&m.x[j].lo,&m.x[j].hi)==2);
  assert(scanf("%x",&f)==1);
  for(unsigned j=0;j<64;j++){unsigned byte;assert(scanf("%2x",&byte)==1);memory[j]=(uint8_t)byte;}
  m.zf=f&1;m.sf=(f>>1)&1;m.cf=(f>>2)&1;m.of=(f>>3)&1;m.pf=(f>>4)&1;m.df=(f>>5)&1;
  assert(rosemary_run(&m,1)==1);
  printf("%" PRIx64,m.rip);
  for(unsigned j=0;j<16;j++)printf(" %" PRIx64,m.r[j]);
  for(unsigned j=0;j<16;j++)printf(" %" PRIx64 " %" PRIx64,m.x[j].lo,m.x[j].hi);
  printf(" %x ",m.zf|(m.sf<<1)|(m.cf<<2)|(m.of<<3)|(m.pf<<4)|(m.df<<5));
  for(unsigned j=0;j<64;j++)printf("%02x",memory[j]);putchar('\n');
 }
 return 0;
}
"#,
    );
    let boundaries = [
        0,
        u128::MAX,
        1,
        1 << 127,
        0x8000_7fff_ffff_0001_7fff_8000_0000_ffff,
        0x80ff_007f_0181_fe02_7f80_00ff_01fe_8203,
    ];
    let mut seed = 7u64;
    let mut input = String::new();
    let mut expected = String::new();
    for i in &p.instructions {
        for n in 0..64 {
            let mut m = Machine::default();
            m.state.rip = i.address;
            for r in &mut m.state.registers {
                seed = seed.wrapping_mul(6364136223846793005).wrapping_add(1);
                *r = seed;
            }
            for v in &mut m.state.xmm {
                seed = seed.wrapping_mul(6364136223846793005).wrapping_add(1);
                let lo = seed;
                seed = seed.wrapping_mul(6364136223846793005).wrapping_add(1);
                *v = u128::from(lo) | (u128::from(seed) << 64);
            }
            if n < 36 {
                m.state.xmm[0] = boundaries[n % 6];
                m.state.xmm[1] = boundaries[n / 6];
            }
            m.state.registers[3] = 0x2003;
            m.state.flags = Flags {
                zf: n & 1 != 0,
                sf: n & 2 != 0,
                cf: n & 4 != 0,
                of: n & 8 != 0,
                pf: n & 16 != 0,
                df: n & 32 != 0,
            };
            m.memory
                .map(0x2000, (0..64).map(|j| (j * 13 + n) as u8).collect(), true)
                .unwrap();
            snapshot(&mut input, &m);
            let report = execute(&p, &mut m);
            assert!(
                matches!(report.stop, Stop::Budget | Stop::LeftCode),
                "{}: {:?}",
                i.op,
                report.stop
            );
            snapshot(&mut expected, &m);
        }
    }
    let dir = std::env::temp_dir().join(format!("rosemary-dynamic-vectors-{}", std::process::id()));
    fs::create_dir_all(&dir).unwrap();
    fs::write(dir.join("test.c"), c).unwrap();
    fs::write(dir.join("input"), input).unwrap();
    let result = Command::new("cc")
        .args([
            "-std=c11",
            "-O1",
            "-Wall",
            "-Wextra",
            "-Werror",
            "-Wno-misleading-indentation",
        ])
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
            "{:?}, input {}",
            p.instructions[n / 64],
            n % 64
        );
    }
    fs::remove_dir_all(dir).unwrap();
}

#[test]
fn vector_faults_are_atomic_and_successful_reads_remain_observable() {
    for store in [false, true] {
        let line = if store {
            "m:128:-:-:1:2000 x:128:0"
        } else {
            "x:128:0 m:128:-:-:1:2000"
        };
        let p = Program::from_text(&format!(
            "rosemary-ir 1\nentry 1000\n1000 1 movdqu {line}\n"
        ))
        .unwrap();
        let mut m = Machine::with_stack(p.entry);
        m.state.xmm[0] = u128::MAX;
        m.memory.map(0x2000, vec![17; 8], true).unwrap();
        let before = m.state;
        let r = execute(&p, &mut m);
        assert!(matches!(r.stop, Stop::MemoryFault(_)));
        assert_eq!(m.state, before);
        assert_eq!(r.memory_reads, u64::from(!store));
        assert_eq!(r.memory_writes, 0);
        assert_eq!(m.memory.load(0x2000, 8).unwrap(), 0x1111_1111_1111_1111);
        m.memory.map(0x2008, vec![34; 8], !store).unwrap();
        if store {
            assert!(matches!(execute(&p, &mut m).stop, Stop::MemoryFault(_)));
            assert_eq!(m.state, before);
            assert_eq!(m.memory.load(0x2000, 8).unwrap(), 0x1111_1111_1111_1111);
        } else {
            let r = execute(&p, &mut m);
            assert_eq!(r.stop, Stop::LeftCode);
            assert_eq!(r.memory_reads, 2);
            assert_eq!(m.state.xmm[0], 0x2222_2222_2222_2222_1111_1111_1111_1111);
            assert!(
                r.json()
                    .contains("\"xmm0\":\"0x22222222222222221111111111111111\"")
            );
            assert!(r.text().contains(
                "xmm0: 0xffffffffffffffffffffffffffffffff -> 0x22222222222222221111111111111111"
            ));
        }
    }
}

#[test]
fn packed_boundaries_include_full_count_width_and_multiply_add_overflow() {
    let p = Program::from_text(
        "rosemary-ir 1\nentry 1000\n1000 1 psraw x:128:0 x:128:1\n1001 1 pmaddwd x:128:0 x:128:0\n",
    )
    .unwrap();
    let mut m = Machine::with_stack(p.entry);
    m.state.xmm[0] = 0x8000_7fff_ffff_0001_8000_0000_ffff_7fff;
    // Bits above the low qword are ignored; bit 32 of the count is significant.
    m.state.xmm[1] = (u128::MAX << 64) | (1 << 32);
    execute(&p, &mut m);
    assert_eq!(m.state.xmm[0], 0xffff_0000_ffff_0000_ffff_0000_ffff_0000);
    m.state.xmm[0] = 0x8000_8000_8000_8000_8000_8000_8000_8000;
    execute(&p, &mut m);
    assert_eq!(m.state.xmm[0], 0x8000_0000_8000_0000_8000_0000_8000_0000);
    m.state.rip = p.entry;
    m.state.xmm[1] = (u128::MAX << 64) | 1;
    execute(&p, &mut m);
    assert_eq!(m.state.xmm[0], 0xc000_0000_c000_0000_c000_0000_c000_0000);
}

#[test]
fn decoded_vector_example_executes_to_a_scalar_result() {
    let bytes = rosemary::input::hex(include_str!("../examples/vector_sad.hex")).unwrap();
    let p = rosemary::decode::decode(&bytes, 0x1000, None).unwrap();
    assert_eq!(
        p,
        Program::from_text(include_str!("../examples/vector_sad.ir")).unwrap()
    );
    let mut m = Machine::with_stack(p.entry);
    m.state.registers[7] = 0x2000;
    m.state.registers[6] = 0x3000;
    m.memory.map(0x2000, (0..16).collect(), false).unwrap();
    m.memory.map(0x3000, vec![0; 16], false).unwrap();
    let r = dynamic::execute(&p, &mut m, Options::default()).unwrap();
    assert_eq!(r.stop, Stop::LeftCode);
    assert_eq!(m.state.registers[0], 120);
    assert_eq!(m.state.rip, u64::MAX);
    assert_eq!(r.steps, 8);
    assert_eq!(r.memory_reads, 5);
    assert_eq!(r.memory_writes, 0);
    assert!(r.trace.iter().all(|step| step.completed));
}
