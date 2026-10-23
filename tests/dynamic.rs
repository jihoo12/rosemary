use rosemary::{
    dynamic::{self, Flags, Machine, Memory, Options, Stop},
    emit,
    ir::{Instruction, Operand, Program},
};
use std::{fmt::Write, fs, process::Command};

fn program(lines: &str) -> Program {
    Program::from_text(&format!("rosemary-ir 1\nentry 1000\n{lines}\n")).unwrap()
}
fn run(p: &Program, m: &mut Machine, max_steps: u64) -> dynamic::Report {
    dynamic::execute(
        p,
        m,
        Options {
            max_steps,
            trace_limit: 100,
        },
    )
    .unwrap()
}

#[test]
fn examples_recover_concrete_paths_calls_and_coverage() {
    for (source, answer) in [
        (include_str!("../examples/sum.ir"), 15),
        (include_str!("../examples/recovery.ir"), 42),
        (include_str!("../examples/proven_branch.ir"), 42),
    ] {
        let p = Program::from_text(source).unwrap();
        let mut m = Machine::with_stack(p.entry);
        let r = run(&p, &mut m, 100);
        assert_eq!(r.stop, Stop::LeftCode);
        assert_eq!(m.state.registers[0], answer);
        assert_eq!(m.state.rip, u64::MAX);
        assert_eq!(m.state.registers[4], 0x8000_0000);
        assert!(r.trace.iter().all(|s| s.completed));
        assert_eq!(r.trace.len() as u64, r.steps);
        if source.contains("xor") {
            assert_eq!(r.trace[1].taken, Some(false));
            assert!(r.counts.len() < p.instructions.len());
        }
    }
}

#[test]
fn budgets_resume_and_trace_limits_do_not_change_execution() {
    let p = program("1000 1 inc r:64:0\n1001 1 jmp i:64:1000");
    let mut m = Machine::with_stack(p.entry);
    let r = dynamic::execute(
        &p,
        &mut m,
        Options {
            max_steps: 7,
            trace_limit: 2,
        },
    )
    .unwrap();
    assert_eq!(r.stop, Stop::Budget);
    assert_eq!(r.steps, 7);
    assert_eq!(r.trace.len(), 2);
    assert_eq!(r.counts[&0x1000], 4);
    assert_eq!(m.state.registers[0], 4);
    let r = run(&p, &mut m, 3);
    assert_eq!(r.stop, Stop::Budget);
    assert_eq!(m.state.registers[0], 5);
    assert!(
        dynamic::execute(
            &p,
            &mut m,
            Options {
                max_steps: 0,
                trace_limit: 0
            }
        )
        .is_err()
    );
    let p = program("1000 1 nop");
    let mut m = Machine::with_stack(p.entry);
    assert_eq!(run(&p, &mut m, 1).stop, Stop::LeftCode);
}

#[test]
fn explicit_memory_maps_check_bounds_permissions_and_atomic_stores() {
    let mut memory = Memory::default();
    memory.map(0x100, vec![0x11, 0x22], true).unwrap();
    memory.map(0x102, vec![0x33, 0x44], false).unwrap();
    assert_eq!(memory.load(0x100, 4).unwrap(), 0x44332211);
    assert!(memory.store(0x100, 4, 0).is_err());
    assert_eq!(memory.load(0x100, 4).unwrap(), 0x44332211);
    assert!(memory.map(0x101, vec![0], true).is_err());
    assert!(memory.map(0xff, vec![0; 8], true).is_err());
    assert!(memory.map(0, vec![], true).is_err());
    assert!(memory.map(u64::MAX, vec![0], true).is_err());
    memory.map(u64::MAX - 1, vec![7], true).unwrap();
    assert_eq!(memory.load(u64::MAX - 1, 1).unwrap(), 7);
    assert!(memory.load(u64::MAX - 1, 4).is_err());
    assert!(memory.load(0x100, 3).is_err());
    assert!(memory.load(0x103, 2).is_err());
}

#[test]
fn faults_roll_back_flags_stack_and_memory_and_can_be_resumed() {
    let p = program("1000 1 add m:64:-:-:1:2000 i:64:1\n1001 1 ret");
    let mut m = Machine::with_stack(p.entry);
    m.memory.map(0x2000, vec![255; 8], false).unwrap();
    let before = m.state;
    let r = run(&p, &mut m, 10);
    assert!(matches!(r.stop, Stop::MemoryFault(_)));
    assert_eq!(m.state, before);
    assert_eq!(r.memory_reads, 1);
    assert_eq!(r.memory_writes, 0);
    assert!(!r.trace[0].completed);
    assert_eq!(m.memory.load(0x2000, 8).unwrap(), u64::MAX);

    let p = program("1000 1 call i:64:1001\n1001 1 nop");
    m.state.registers[4] = 0x3008;
    let before = m.state;
    assert!(matches!(run(&p, &mut m, 10).stop, Stop::MemoryFault(_)));
    assert_eq!(m.state, before);
    m.memory.map(0x3000, vec![0; 8], true).unwrap();
    assert_eq!(run(&p, &mut m, 10).stop, Stop::LeftCode);
    assert_eq!(m.memory.load(0x3000, 8).unwrap(), 0x1001);
}

#[test]
fn indirect_calls_segmented_memory_and_pop_use_concrete_addresses() {
    let p = program(
        "1000 1 mov r:64:3 i:64:1005\n1001 1 notrack.call r:64:3\n1002 1 pop m:64:4:-:1:0\n1003 1 mov r:64:0 m:64:-:-:1:10:fs\n1004 1 jmp i:64:2000\n1005 1 ret",
    );
    let mut m = Machine::with_stack(p.entry);
    m.state.fs_base = 0x3000;
    m.memory
        .map(0x3010, 42u64.to_le_bytes().to_vec(), true)
        .unwrap();
    // POP's destination is evaluated after RSP advances.
    m.state.registers[4] -= 8;
    m.memory.store(m.state.registers[4], 8, 0x1234).unwrap();
    let r = run(&p, &mut m, 20);
    assert_eq!(r.stop, Stop::LeftCode);
    assert_eq!(m.state.registers[0], 42);
    assert_eq!(m.memory.load(0x7fff_fff8, 8).unwrap(), 0x1234);
    assert_eq!(r.edges[&(0x1001, 0x1005)], 1);
    assert_eq!(r.edges[&(0x1005, 0x1002)], 1);
}

#[test]
fn rep_is_bounded_resumable_directional_and_faults_per_element() {
    let p = program("1000 1 rep.movs i:8:8\n1001 1 nop");
    let mut m = Machine::with_stack(p.entry);
    m.memory.map(0x2000, vec![1, 2, 3, 4], true).unwrap();
    m.memory.map(0x3000, vec![0; 3], true).unwrap();
    m.state.registers[1] = 4;
    m.state.registers[6] = 0x2000;
    m.state.registers[7] = 0x3000;
    assert_eq!(run(&p, &mut m, 2).stop, Stop::Budget);
    assert_eq!(m.state.registers[1], 2);
    assert_eq!(m.memory.load(0x3000, 2).unwrap(), 0x0201);
    let r = run(&p, &mut m, 10);
    assert!(matches!(r.stop, Stop::MemoryFault(_)));
    assert_eq!(m.state.registers[1], 1);
    assert_eq!(m.state.registers[6], 0x2003);
    m.memory.map(0x3003, vec![0], true).unwrap();
    assert_eq!(run(&p, &mut m, 10).stop, Stop::LeftCode);
    assert_eq!(m.memory.load(0x3000, 4).unwrap(), 0x04030201);
    m.state.rip = p.entry;
    m.state.flags.df = true;
    m.state.registers[1] = 4;
    m.state.registers[6] = 0x2003;
    m.state.registers[7] = 0x3003;
    assert_eq!(run(&p, &mut m, 10).stop, Stop::LeftCode);
    assert_eq!(m.state.registers[6], 0x1fff);
    m.state.rip = p.entry;
    // RCX=0 bypasses even invalid memory addresses.
    m.state.registers[6] = u64::MAX;
    assert_eq!(run(&p, &mut m, 10).memory_reads, 0);
}

#[test]
fn division_faults_and_128_bit_signed_boundaries_are_explicit() {
    let p = program("1000 1 idiv r:64:1");
    for (hi, lo, den, expected) in [
        (
            u64::MAX,
            (-10i64) as u64,
            3,
            Some(((-3i64) as u64, u64::MAX)),
        ),
        (0, 10, (-3i64) as u64, Some(((-3i64) as u64, 1))),
        (u64::MAX, 1 << 63, u64::MAX, None),
        (1 << 63, 0, u64::MAX, None),
        (0, 42, 0, None),
    ] {
        let mut m = Machine::with_stack(p.entry);
        m.state.registers[0] = lo;
        m.state.registers[2] = hi;
        m.state.registers[1] = den;
        let before = m.state;
        let r = run(&p, &mut m, 1);
        if let Some((q, rem)) = expected {
            assert_eq!(r.stop, Stop::LeftCode);
            assert_eq!((m.state.registers[0], m.state.registers[2]), (q, rem));
        } else {
            assert!(matches!(r.stop, Stop::Trap(_)));
            assert_eq!(m.state, before);
        }
    }
}

#[test]
fn traps_and_unsupported_vectors_preserve_the_faulting_address() {
    for (line, stop) in [
        ("ud2", Stop::Trap("ud2".into())),
        ("pxor x:128:0 x:128:0", Stop::Unsupported("pxor".into())),
    ] {
        let p = program(&format!("1000 1 {line}"));
        let mut m = Machine::with_stack(p.entry);
        let r = run(&p, &mut m, 2);
        assert_eq!(r.stop, stop);
        assert_eq!(r.final_state.rip, p.entry);
        assert_eq!(r.steps, 1);
    }
}

fn cli(args: &[&str], source: &str) -> std::process::Output {
    use std::{io::Write, process::Stdio};
    let mut child = Command::new(env!("CARGO_BIN_EXE_rosemary"))
        .args(args)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    child
        .stdin
        .take()
        .unwrap()
        .write_all(source.as_bytes())
        .unwrap();
    child.wait_with_output().unwrap()
}

#[test]
fn cli_initializes_inputs_reports_json_and_rejects_invalid_options() {
    let source = "rosemary-ir 1\nentry 1000\n1000 1 adc r:64:0 m:64:7:-:1:0\n1001 1 ret\n";
    let result = cli(
        &[
            "trace",
            "-",
            "--reg",
            "rax=0x10",
            "--reg",
            "rdi=0x2000",
            "--flag",
            "cf=1",
            "--memory",
            "0x2000:1900000000000000",
            "--to",
            "json",
            "--trace-limit",
            "0",
        ],
        source,
    );
    assert!(
        result.status.success(),
        "{}",
        String::from_utf8_lossy(&result.stderr)
    );
    let report = String::from_utf8(result.stdout).unwrap();
    assert!(report.contains("\"schema\":\"rosemary-trace-1\""));
    assert!(report.contains("\"rax\":\"0x2a\""));
    assert!(report.contains("\"trace_truncated\":true,\"trace\":[]"));
    assert!(report.contains("\"stop\":\"left-code\""));
    for args in [
        vec!["trace", "-", "--max-steps", "0"],
        vec!["trace", "-", "--max-steps", "invalid"],
        vec!["trace", "-", "--reg", "eax=1"],
        vec!["trace", "-", "--flag", "cf=2"],
        vec!["trace", "-", "--flag", "af=0"],
        vec!["trace", "-", "--map", "0x2000:0"],
        vec!["trace", "-", "--map", "0x2000:67108864"],
        vec!["trace", "-", "--map", "0x7fff0000:8"],
        vec!["trace", "-", "--memory", "0x2000:zz"],
        vec!["trace", "-", "--to", "dot"],
        vec!["trace", "-", "--recursive"],
        vec!["analyze", "-", "--reg", "rax=1"],
        vec!["lift", "--hex", "c3", "--max-steps", "10"],
    ] {
        let result = cli(&args, source);
        assert!(!result.status.success(), "{args:?}");
        assert!(result.stdout.is_empty(), "{args:?}");
    }
}

#[test]
fn concrete_integer_state_matches_generated_c_across_boundary_inputs() {
    let mut p = Program {
        entry: 0x4000,
        instructions: Vec::new(),
    };
    let mut add = |op: &str, operands: Vec<Operand>| {
        p.instructions.push(Instruction {
            address: 0x4000 + p.instructions.len() as u64,
            len: 1,
            op: op.into(),
            operands,
        });
    };
    for w in [8, 16, 32, 64] {
        for op in [
            "mov", "add", "adc", "sub", "sbb", "cmp", "test", "xor", "and", "or", "xchg", "xadd",
            "cmpxchg", "shl", "shr", "sar", "rol", "ror", "rcl", "rcr",
        ] {
            let count = matches!(op, "shl" | "shr" | "sar" | "rol" | "ror" | "rcl" | "rcr");
            add(
                op,
                vec![
                    Operand::Reg(0, w),
                    Operand::Reg(1, if count { 8 } else { w }),
                ],
            );
        }
        for op in ["inc", "dec", "neg", "not", "mul", "imul", "div", "idiv"] {
            add(op, vec![Operand::Reg(1, w)]);
        }
        if w >= 16 {
            for op in [
                "imul", "bsf", "bsr", "tzcnt", "lzcnt", "popcnt", "bt", "bts", "btr", "btc",
            ] {
                add(op, vec![Operand::Reg(0, w), Operand::Reg(1, w)]);
            }
            for op in ["shld", "shrd"] {
                add(
                    op,
                    vec![Operand::Reg(0, w), Operand::Reg(2, w), Operand::Reg(1, 8)],
                );
            }
            for op in ["movsx", "movzx"] {
                add(op, vec![Operand::Reg(0, w), Operand::High(1)]);
            }
        }
        let mem = Operand::Mem {
            segment: None,
            width: w,
            base: Some(3),
            index: None,
            scale: 1,
            disp: 0,
        };
        for op in ["mov", "add", "adc", "sub", "sbb", "xadd", "cmpxchg", "xchg"] {
            add(op, vec![mem.clone(), Operand::Reg(0, w)]);
        }
        if w >= 32 {
            add("bswap", vec![Operand::Reg(0, w)]);
        }
    }
    for op in [
        "clc", "stc", "cmc", "cld", "std", "cwd", "cdq", "cqo", "leave", "ret",
    ] {
        add(op, vec![]);
    }
    add("ret", vec![Operand::Imm(0xffff, 16)]);
    for op in ["push", "pop", "call", "jmp"] {
        add(op, vec![Operand::Reg(0, 64)]);
    }
    for op in rosemary::ir::CONDITIONS {
        add(op, vec![Operand::Imm(0x8000, 64)]);
        add(
            &format!("cmov{}", &op[1..]),
            vec![Operand::Reg(0, 32), Operand::Reg(1, 32)],
        );
        add(&format!("set{}", &op[1..]), vec![Operand::High(0)]);
    }
    for op in ["loop", "loope", "loopne", "jrcxz"] {
        add(op, vec![Operand::Imm(0x8000, 64)]);
    }
    for w in [8, 16, 32, 64] {
        for op in ["movs", "stos", "rep.movs", "rep.stos"] {
            add(op, vec![Operand::Imm(w, 8)]);
        }
    }
    p.validate().unwrap();
    let mut c = emit::c(&p).unwrap();
    c.push_str(
        r#"
#include <stdio.h>
#include <inttypes.h>
#include <assert.h>
static uint64_t load(void *ctx,uint64_t a,unsigned n) {
 assert(a>=0x1000&&a+n<=0x1100); uint8_t *b=ctx;uint64_t v=0;
 for(unsigned j=0;j<n;j++)v|=(uint64_t)b[a-0x1000+j]<<(8*j);return v;
}
static void store(void *ctx,uint64_t a,unsigned n,uint64_t v) {
 assert(a>=0x1000&&a+n<=0x1100);uint8_t *b=ctx;
 for(unsigned j=0;j<n;j++)b[a-0x1000+j]=(uint8_t)(v>>(8*j));
}
int main(void) {
 uint64_t rip;
 while(scanf("%" SCNu64,&rip)==1) {
  rosemary_machine m={0};uint8_t memory[256];unsigned f,seed;
  m.rip=rip;m.load=load;m.store=store;m.ctx=memory;
  for(unsigned j=0;j<16;j++)assert(scanf("%" SCNu64,&m.r[j])==1);
  assert(scanf("%u%u",&f,&seed)==2);
  m.zf=f&1;m.sf=(f>>1)&1;m.cf=(f>>2)&1;m.of=(f>>3)&1;m.pf=(f>>4)&1;m.df=(f>>5)&1;
  for(unsigned j=0;j<256;j++)memory[j]=(uint8_t)(j*13+seed);
  int status=rosemary_run(&m,1);
  printf("%d %" PRIu64,status,m.rip);
  for(unsigned j=0;j<16;j++)printf(" %" PRIu64,m.r[j]);
  printf(" %u ",m.zf|(m.sf<<1)|(m.cf<<2)|(m.of<<3)|(m.pf<<4)|(m.df<<5));
  for(unsigned j=0;j<256;j++)printf("%02x",memory[j]);
  putchar('\n');
 }
 return 0;
}
"#,
    );
    let mut input = String::new();
    let mut expected = String::new();
    let mut seed = 7u64;
    let boundaries = [
        0,
        1,
        2,
        7,
        8,
        9,
        16,
        17,
        31,
        32,
        63,
        64,
        0x7f,
        0x80,
        0xff,
        0x7fff,
        0x8000,
        0xffff,
        0x7fff_ffff,
        0x8000_0000,
        0xffff_ffff,
        (1u64 << 63) - 1,
        1u64 << 63,
        u64::MAX,
    ];
    for i in &p.instructions {
        for n in 0..64 {
            let mut m = Machine::default();
            m.state.rip = i.address;
            for reg in &mut m.state.registers {
                seed = seed.wrapping_mul(6364136223846793005).wrapping_add(1);
                *reg = seed;
            }
            m.state.registers[0] = boundaries[n % boundaries.len()];
            m.state.registers[1] = boundaries[(n * 7) % boundaries.len()];
            for (r, value) in [
                (3, 0x1080),
                (4, 0x10e0),
                (5, 0x10b0),
                (6, 0x1040),
                (7, 0x10c0),
            ] {
                m.state.registers[r] = value;
            }
            m.state.flags = Flags {
                zf: n & 1 != 0,
                sf: n & 2 != 0,
                cf: n & 4 != 0,
                of: n & 8 != 0,
                pf: n & 16 != 0,
                df: n & 32 != 0,
            };
            let bytes: Vec<u8> = (0..256).map(|j| (j * 13 + n) as u8).collect();
            m.memory.map(0x1000, bytes, true).unwrap();
            write!(input, "{}", i.address).unwrap();
            for reg in m.state.registers {
                write!(input, " {reg}").unwrap();
            }
            writeln!(input, " {n} {n}").unwrap();
            let r = dynamic::execute(
                &p,
                &mut m,
                Options {
                    max_steps: 1,
                    trace_limit: 0,
                },
            )
            .unwrap();
            assert!(
                !matches!(r.stop, Stop::MemoryFault(_) | Stop::Unsupported(_)),
                "{}: {:?}",
                i.op,
                r.stop
            );
            write!(
                expected,
                "{} {}",
                if matches!(r.stop, Stop::Trap(_)) {
                    2
                } else {
                    1
                },
                m.state.rip
            )
            .unwrap();
            for reg in m.state.registers {
                write!(expected, " {reg}").unwrap();
            }
            let f = m.state.flags;
            let bits = u8::from(f.zf)
                | (u8::from(f.sf) << 1)
                | (u8::from(f.cf) << 2)
                | (u8::from(f.of) << 3)
                | (u8::from(f.pf) << 4)
                | (u8::from(f.df) << 5);
            write!(expected, " {bits} ").unwrap();
            for j in 0..256 {
                write!(expected, "{:02x}", m.memory.load(0x1000 + j, 1).unwrap()).unwrap();
            }
            expected.push('\n');
        }
    }
    let dir = std::env::temp_dir().join(format!("rosemary-dynamic-{}", std::process::id()));
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
    for (index, (a, e)) in actual.lines().zip(expected.lines()).enumerate() {
        assert_eq!(
            a,
            e,
            "instruction {:?}, input {}",
            p.instructions[index / 64],
            index % 64
        );
    }
    fs::remove_dir_all(dir).unwrap();
}
