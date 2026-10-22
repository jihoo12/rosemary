use rosemary::{
    analysis::{self, EdgeKind, Resolution},
    dataflow::{self, Flags, State},
    decode::{self, RecursiveOptions},
    emit, input,
    ir::{Operand, Program},
};
#[cfg(target_arch = "x86_64")]
use std::fmt::Write;
use std::{fs, path::Path, process::Command};

fn returns() -> Program {
    decode::decode(
        &input::hex("c3 c2 00 00 c2 08 00 c2 10 00 c2 ff ff").unwrap(),
        0x1000,
        None,
    )
    .unwrap()
}

#[test]
fn ret_immediate_roundtrips_and_terminates_recursive_paths() {
    let p = returns();
    assert_eq!(Program::from_text(&p.to_text().unwrap()).unwrap(), p);
    assert!(p.instructions[0].operands.is_empty());
    for (i, extra) in p.instructions[1..].iter().zip([0, 8, 16, 65535]) {
        assert_eq!(i.op, "ret");
        assert_eq!(i.len, 3);
        assert_eq!(i.operands, [Operand::Imm(extra, 16)]);
        assert!(!i.falls_through());
        assert_eq!(i.direct_target(), None);
    }
    let bytes = input::hex("e8 01 00 00 00 c3 c2 10 00 0f 05").unwrap();
    let p = decode::decode_recursive(&bytes, 0x1000, 0x1000, RecursiveOptions::default()).unwrap();
    assert_eq!(p.instructions.len(), 3);
    let a = analysis::analyze(&p).unwrap();
    assert_eq!(a.function_entries, [0x1000, 0x1006]);
    assert_eq!(
        a.edges
            .iter()
            .filter(|e| e.kind == EdgeKind::Return)
            .count(),
        2
    );
    assert!(
        a.edges
            .iter()
            .filter(|e| e.kind == EdgeKind::Return)
            .all(|e| e.target.is_none() && e.resolution == Resolution::Return)
    );
    assert!(decode::decode(&bytes, 0x1000, None).is_err());
}

#[test]
fn invalid_return_operands_and_truncated_immediates_are_rejected() {
    for operands in [
        "i:8:8",
        "i:32:8",
        "i:64:8",
        "i:16:10000",
        "r:16:0",
        "m:16:0:-:1:0",
        "i:16:1 i:16:2",
    ] {
        assert!(
            Program::from_text(&format!("rosemary-ir 1\nentry 0\n0 3 ret {operands}\n")).is_err(),
            "{operands}"
        );
    }
    for hex in [
        "c2",
        "c2 00",
        "66 c2 10 00",
        "f0 c2 10 00",
        "f3 c2 10 00",
        "c2 10 00 0f 05",
    ] {
        let bytes = input::hex(hex).unwrap();
        assert!(decode::decode(&bytes, 0, None).is_err(), "{hex}");
    }
    for hex in ["c2", "c2 00"] {
        let bytes = input::hex(hex).unwrap();
        assert!(
            decode::decode_recursive(&bytes, 0, 0, RecursiveOptions::default())
                .unwrap_err()
                .contains("0x0: truncated instruction")
        );
    }
    assert!(decode::decode(&[0xc2, 0, 0], u64::MAX - 2, None).is_err());
}

#[test]
fn return_value_analysis_accounts_for_unsigned_cleanup_and_preserves_flags() {
    let p = returns();
    for (i, extra) in p.instructions.iter().zip([0, 0, 8, 16, 65535]) {
        for stack in [0, 16, u64::MAX - 7, u64::MAX] {
            let mut before = State {
                registers: [Some(0x1234); 16],
                flags: Flags {
                    cf: Some(true),
                    sf: Some(false),
                    zf: Some(true),
                    of: Some(false),
                    pf: Some(true),
                },
                direction: Some(true),
            };
            before.registers[4] = Some(stack);
            let after = dataflow::transfer(i, before);
            let mut expected = before;
            expected.registers[4] = Some(stack.wrapping_add(8 + extra));
            assert_eq!(after, expected);
        }
        assert_eq!(dataflow::transfer(i, State::default()), State::default());
    }
    let p =
        Program::from_text("rosemary-ir 1\nentry 0\n0 1 mov r:64:4 i:64:100\n1 3 ret i:16:10\n")
            .unwrap();
    let a = dataflow::analyze(&p).unwrap();
    assert!(a.json().contains("\"rsp\": \"0x118\""));
    assert_eq!(a.simplify().unwrap(), p);
}

fn compile_run(dir: &Path, assembly: bool) {
    let mut command = Command::new("cc");
    command
        .args(["-std=c11", "-O2", "-Wall", "-Wextra", "-Werror"])
        .arg(dir.join("test.c"));
    if assembly {
        command.arg(dir.join("test.s"));
    }
    let output = command.arg("-o").arg(dir.join("test")).output().unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(Command::new(dir.join("test")).status().unwrap().success());
}

#[test]
fn generated_c_pops_before_cleanup_wraps_and_resumes_with_instruction_budgets() {
    let p = returns();
    let mut c = emit::c(&p).unwrap();
    c.push_str(
        r#"
#include <assert.h>
typedef struct { uint64_t expected, target; unsigned loads; } memory;
static uint64_t load(void *ctx, uint64_t address, unsigned width) {
    memory *mem = ctx;
    assert(address == mem->expected && width == 8);
    mem->loads++;
    return mem->target;
}
int main(void) {
    const uint64_t sites[] = {0x1000, 0x1001, 0x1004, 0x1007, 0x100a};
    const unsigned cleanup[] = {0, 0, 8, 16, 65535};
    const uint64_t stacks[] = {0, 128, UINT64_MAX - 7, UINT64_MAX};
    for (unsigned j = 0; j < 5; ++j) for (unsigned k = 0; k < 4; ++k) {
        memory mem = {stacks[k], UINT64_C(0xdeadbeef), 0};
        rosemary_machine m = {0};
        m.load = load; m.ctx = &mem; m.rip = sites[j];
        for (unsigned r = 0; r < 16; ++r) m.r[r] = 0x1234 + r;
        m.r[4] = stacks[k];
        m.cf = m.of = m.zf = m.sf = m.pf = m.df = 1;
        assert(rosemary_run(&m, 0) == 1 && !mem.loads && m.rip == sites[j]);
        assert(rosemary_run(&m, 1) == 1 && mem.loads == 1 && m.rip == mem.target);
        assert(m.r[4] == stacks[k] + 8 + cleanup[j]);
        for (unsigned r = 0; r < 16; ++r) if (r != 4) assert(m.r[r] == 0x1234 + r);
        assert(m.cf && m.of && m.zf && m.sf && m.pf && m.df);
        assert(rosemary_run(&m, 1) == 0 && mem.loads == 1);
    }
    /* Return to another translated instruction, then resume with a fresh budget. */
    memory mem = {128, 0x1000, 0};
    rosemary_machine m = {0};
    m.load = load; m.ctx = &mem; m.rip = 0x1007; m.r[4] = 128;
    assert(rosemary_run(&m, 1) == 1 && m.rip == 0x1000 && m.r[4] == 152);
    mem.expected = 152; mem.target = 0xdeadbeef;
    assert(rosemary_run(&m, 2) == 0 && m.r[4] == 160 && mem.loads == 2);
    return 0;
}
"#,
    );
    let dir = std::env::temp_dir().join(format!("rosemary-return-c-{}", std::process::id()));
    fs::create_dir_all(&dir).unwrap();
    fs::write(dir.join("test.c"), c).unwrap();
    compile_run(&dir, false);
    fs::remove_dir_all(dir).unwrap();
}

#[test]
#[cfg(target_arch = "x86_64")]
fn reconstructed_returns_match_native_stack_cleanup() {
    let p = returns();
    let mut assembly = emit::assembly(&p).unwrap();
    let mut c = String::from("#include <stdint.h>\n#include <assert.h>\n");
    for (n, (i, extra)) in p.instructions.iter().zip([0, 0, 8, 16, 65535]).enumerate() {
        // Keep the caller's stack in a callee-saved register, allocate the
        // discarded argument area, and compare the stack pointer after RET.
        writeln!(assembly, ".global native_ret_{n}\nnative_ret_{n}:\n push rbx\n mov rbx, rsp\n sub rsp, {extra}\n call .L_{:x}\n mov rax, rsp\n sub rax, rbx\n mov rsp, rbx\n pop rbx\n ret", i.address).unwrap();
        writeln!(c, "extern uint64_t native_ret_{n}(void);").unwrap();
    }
    assembly.push_str(".section .note.GNU-stack,\"\",@progbits\n");
    c.push_str("int main(void) {\n");
    for n in 0..p.instructions.len() {
        writeln!(c, "assert(native_ret_{n}() == 0);").unwrap();
    }
    c.push_str("return 0; }\n");
    let dir = std::env::temp_dir().join(format!("rosemary-return-native-{}", std::process::id()));
    fs::create_dir_all(&dir).unwrap();
    fs::write(dir.join("test.c"), c).unwrap();
    fs::write(dir.join("test.s"), assembly).unwrap();
    compile_run(&dir, true);
    fs::remove_dir_all(dir).unwrap();
}
