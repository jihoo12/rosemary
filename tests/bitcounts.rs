use rosemary::{
    dataflow::{self, Flags, State},
    decode, emit, input,
    ir::{Operand, Program},
};
use std::{fs, process::Command};

#[test]
fn bit_counts_decode_all_widths_prefix_orders_and_extended_operands() {
    for (opcode, op) in [("b8", "popcnt"), ("bc", "tzcnt"), ("bd", "lzcnt")] {
        for (prefix, width) in [
            ("66 f3", 16),
            ("f3 66", 16),
            ("f3", 32),
            ("f3 48", 64),
            ("66 f3 48", 64),
        ] {
            let bytes = input::hex(&format!("{prefix} 0f {opcode} c1")).unwrap();
            let p = decode::decode(&bytes, 0x1000, None).unwrap();
            assert_eq!(p.instructions[0].op, op);
            assert_eq!(p.instructions[0].len as usize, bytes.len());
            assert_eq!(
                p.instructions[0].operands,
                [Operand::Reg(0, width), Operand::Reg(1, width)]
            );
            assert_eq!(Program::from_text(&p.to_text().unwrap()).unwrap(), p);
        }
        let bytes = input::hex(&format!("f3 4d 0f {opcode} c1")).unwrap();
        let p = decode::decode(&bytes, 0, None).unwrap();
        assert_eq!(
            p.instructions[0].operands,
            [Operand::Reg(8, 64), Operand::Reg(9, 64)]
        );
    }
    let p = decode::decode(&input::hex("64 f3 66 44 0f b8 4b f8").unwrap(), 0, None).unwrap();
    assert!(
        emit::assembly(&p)
            .unwrap()
            .contains("popcnt r9w, word ptr fs:[rbx")
    );
}

#[test]
fn bit_count_support_keeps_invalid_forms_and_other_prefix_combinations_rejected() {
    for bytes in [
        "0f b8 c1",
        "66 0f b8 c1",
        "f2 0f b8 c1",
        "f2 66 0f b8 c1",
        "f0 f3 0f b8 03",
        "66 f3 0f 6f c1",
        "66 f2 0f 10 c1",
    ] {
        assert!(
            decode::decode(&input::hex(bytes).unwrap(), 0, None).is_err(),
            "{bytes}"
        );
    }
    for operands in [
        "r:8:0 r:8:1",
        "r:16:0 r:32:1",
        "r:64:0 i:64:1",
        "m:64:0:-:1:0 r:64:1",
        "x:128:0 x:128:1",
    ] {
        assert!(
            Program::from_text(&format!("rosemary-ir 1\nentry 0\n0 1 popcnt {operands}\n"))
                .is_err()
        );
    }
    let bytes = input::hex("f3 48 0f b8 84 8b 10 20 30 40").unwrap();
    for n in 0..bytes.len() {
        assert!(decode::decode(&bytes[..n], 0, None).is_err());
    }
}

#[test]
fn popcnt_tracks_known_results_and_clears_flags_even_for_unknown_inputs() {
    for (prefix, width) in [("66 f3", 16), ("f3", 32), ("f3 48", 64)] {
        let p =
            decode::decode(&input::hex(&format!("{prefix} 0f b8 c1")).unwrap(), 0, None).unwrap();
        let mask = u64::MAX >> (64 - width);
        for value in [0, 1, 0x8000, 0x10000, 0x80000000, 0x100000000, u64::MAX] {
            let mut state = State::default();
            state.registers[0] = Some(u64::MAX);
            state.registers[1] = Some(value);
            state.direction = Some(true);
            let after = dataflow::transfer(&p.instructions[0], state);
            let result = (value & mask).count_ones() as u64;
            assert_eq!(
                after.registers[0],
                Some(if width == 16 { !mask | result } else { result })
            );
            assert_eq!(
                after.flags,
                Flags {
                    zf: Some(value & mask == 0),
                    sf: Some(false),
                    cf: Some(false),
                    of: Some(false),
                    pf: Some(false)
                }
            );
            assert_eq!(after.direction, Some(true));
        }
        let after = dataflow::transfer(&p.instructions[0], State::default());
        assert_eq!(after.registers[0], None);
        assert_eq!(
            after.flags,
            Flags {
                zf: None,
                sf: Some(false),
                cf: Some(false),
                of: Some(false),
                pf: Some(false)
            }
        );
    }
    let p = Program::from_text(
        "rosemary-ir 1\nentry 0\n0 1 popcnt r:64:0 r:64:1\n1 1 jb i:64:3\n2 1 ret\n3 1 ret\n",
    )
    .unwrap();
    let a = dataflow::analyze(&p).unwrap();
    assert_eq!(a.branch(1), Some(false));
    assert_eq!(a.simplify().unwrap().instructions[1].op, "nop");
}

#[test]
fn popcnt_generated_c_executes_register_and_memory_forms_without_cpu_features() {
    let p = decode::decode(
        &input::hex("f3 66 0f b8 03 f3 0f b8 c8 f3 48 0f b8 d1").unwrap(),
        0,
        None,
    )
    .unwrap();
    let mut c = emit::c(&p).unwrap();
    c.push_str(
        r#"
#include <assert.h>
static uint64_t load(void *ctx, uint64_t address, unsigned width) {
    assert(address == 128 && width == 2);
    return *(uint64_t *)ctx & 65535;
}
int main(void) {
    for (uint64_t value = 0; value <= 65535; ++value) {
        rosemary_machine m = {0};
        m.ctx = &value; m.load = load; m.r[3] = 128; m.r[0] = UINT64_MAX;
        m.cf = m.of = m.sf = m.pf = m.zf = m.df = 1;
        unsigned count = 0;
        for (unsigned bit = 0; bit < 16; ++bit) count += (value >> bit) & 1;
        assert(rosemary_run(&m, 1) == 1);
        assert(m.r[0] == (UINT64_C(0xffffffffffff0000) | count));
        assert(m.zf == (value == 0) && !m.cf && !m.of && !m.sf && !m.pf && m.df);
        assert(rosemary_run(&m, 2) == 1);
        unsigned middle = 16, final = 0;
        for (unsigned bit = 0; bit < 5; ++bit) middle += (count >> bit) & 1;
        for (unsigned bit = 0; bit < 6; ++bit) final += (middle >> bit) & 1;
        assert(m.r[1] == middle && m.r[2] == final);
    }
    return 0;
}
"#,
    );
    let dir = std::env::temp_dir().join(format!("rosemary-popcnt-{}", std::process::id()));
    fs::create_dir_all(&dir).unwrap();
    fs::write(dir.join("test.c"), c).unwrap();
    let output = Command::new("cc")
        .args(["-std=c11", "-O2", "-Wall", "-Wextra", "-Werror"])
        .arg(dir.join("test.c"))
        .arg("-o")
        .arg(dir.join("test"))
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(Command::new(dir.join("test")).status().unwrap().success());
    fs::write(dir.join("test.s"), emit::assembly(&p).unwrap()).unwrap();
    assert!(
        Command::new("cc")
            .arg("-c")
            .arg(dir.join("test.s"))
            .arg("-o")
            .arg(dir.join("test.o"))
            .status()
            .unwrap()
            .success()
    );
    fs::remove_dir_all(dir).unwrap();
}
