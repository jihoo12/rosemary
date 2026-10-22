use rosemary::{
    decode::decode,
    emit, input,
    ir::{Instruction, Operand, Program},
};
use std::{
    fs,
    io::Write,
    process::{Command, Stdio},
};

#[test]
fn loop_roundtrip_and_outputs() {
    let bytes = input::hex(include_str!("../examples/sum.hex")).unwrap();
    let p = decode(&bytes, 0x1000, None).unwrap();
    assert_eq!(p.instructions.len(), 6);
    assert_eq!(p.instructions[4].operands, vec![Operand::Imm(0x100a, 64)]);
    assert_eq!(Program::from_text(&p.to_text().unwrap()).unwrap(), p);
    let asm = emit::assembly(&p).unwrap();
    assert!(asm.contains("jne .L_100a"));
    assert!(asm.contains("add eax, ecx"));
}
#[test]
fn addressing_immediates_and_register_extensions() {
    let p = decode(
        &input::hex("4f 8b 44 ec f0 48 8d 05 10 00 00 00 49 c7 c0 ff ff ff ff 41 b9 01 00 00 00")
            .unwrap(),
        0x1000,
        None,
    )
    .unwrap();
    assert_eq!(
        p.instructions[0].operands,
        vec![
            Operand::Reg(8, 64),
            Operand::Mem {
                segment: None,
                width: 64,
                base: Some(12),
                index: Some(13),
                scale: 8,
                disp: (-16i64) as u64
            }
        ]
    );
    assert_eq!(
        p.instructions[1].operands[1],
        Operand::Mem {
            segment: None,
            width: 64,
            base: None,
            index: None,
            scale: 1,
            disp: 0x101c
        }
    );
    assert_eq!(p.instructions[2].operands[1], Operand::Imm(u64::MAX, 64));
    assert_eq!(p.instructions[3].operands[0], Operand::Reg(9, 32));
    // SIB index=4 with REX.X selects r12, not the no-index sentinel.
    let p = decode(&input::hex("4a 8b 04 65 00 00 00 00").unwrap(), 0, None).unwrap();
    assert_eq!(
        p.instructions[0].operands[1],
        Operand::Mem {
            segment: None,
            width: 64,
            base: None,
            index: Some(12),
            scale: 2,
            disp: 0
        }
    );
}
#[test]
fn invalid_inputs_are_rejected_without_panics() {
    for bytes in [
        &[0x0f, 0x05][..],
        &[0xb8, 1],
        &[0x48],
        &[0x8d, 0xc0],
        &[0x66, 0xb8, 0],
        &[],
    ] {
        assert!(decode(bytes, 0, None).is_err());
    }
    assert!(decode(&[0x90], u64::MAX, None).is_err());
    assert!(input::hex("0").is_err());
    assert!(input::hex("é").is_err());
    assert!(input::hex("+1").is_err());
    assert!(input::hex("00 +a ff").is_err());
    assert_eq!(input::hex("aB_09 \n\tCd").unwrap(), [0xab, 0x09, 0xcd]);
    assert!(Program::from_text("rosemary-ir 1\nentry 0\n0 1 mov r:64:99 i:64:0\n").is_err());
    assert!(Program::from_text("rosemary-ir 1\nentry 0\n0 2 nop\n1 1 ret\n").is_err());
    assert!(Program::from_text("rosemary-ir 1\nentry 1\n0 1 nop\n").is_err());
    // All truncated prefixes of valid instructions must fail.
    for hex in [
        "48 b8 01 02 03 04 05 06 07 08",
        "4f 8b 84 ec 00 00 00 00",
        "0f 85 01 00 00 00",
    ] {
        let b = input::hex(hex).unwrap();
        for n in 0..b.len() {
            assert!(decode(&b[..n], 0, None).is_err());
        }
    }
    // Exercise decoder and ELF parser with deterministic arbitrary byte strings.
    let mut seed = 1u64;
    for n in 0..1024 {
        let b: Vec<_> = (0..n % 80)
            .map(|_| {
                seed = seed.wrapping_mul(6364136223846793005).wrapping_add(1);
                (seed >> 32) as u8
            })
            .collect();
        let _ = decode(&b, 0, None);
        let _ = input::elf_text(&b);
    }
}
fn elf_fixture() -> Vec<u8> {
    let mut b = vec![0u8; 0x210];
    b[..7].copy_from_slice(b"\x7fELF\x02\x01\x01");
    fn put(b: &mut [u8], off: usize, n: usize, v: u64) {
        b[off..off + n].copy_from_slice(&v.to_le_bytes()[..n]);
    }
    for (o, n, v) in [
        (16, 2, 2),
        (18, 2, 62),
        (20, 4, 1),
        (24, 8, 0x401000),
        (40, 8, 64),
        (52, 2, 64),
        (58, 2, 64),
        (60, 2, 3),
        (62, 2, 1),
        (128, 4, 1),
        (132, 4, 3),
        (152, 8, 0x180),
        (160, 8, 17),
        (192, 4, 11),
        (196, 4, 1),
        (200, 8, 6),
        (208, 8, 0x401000),
        (216, 8, 0x200),
        (224, 8, 6),
    ] {
        put(&mut b, o, n, v);
    }
    b[0x180..0x191].copy_from_slice(b"\0.shstrtab\0.text\0");
    b[0x200..0x206].copy_from_slice(&[0xb8, 42, 0, 0, 0, 0xc3]);
    b
}
#[test]
fn elf_bounds_and_text_selection() {
    let b = elf_fixture();
    let c = input::elf_text(&b).unwrap();
    assert_eq!(c.base, 0x401000);
    assert_eq!(c.entry, c.base);
    assert_eq!(c.bytes, [0xb8, 42, 0, 0, 0, 0xc3]);
    for n in 0..0x206 {
        assert!(input::elf_text(&b[..n]).is_err());
    }
    let mut b = b;
    b[40..48].copy_from_slice(&u64::MAX.to_le_bytes());
    assert!(input::elf_text(&b).is_err());
}
#[test]
fn generated_c_executes_loop_flags_memory_and_calls() {
    let dir = std::env::temp_dir().join(format!("rosemary-tests-{}", std::process::id()));
    fs::create_dir_all(&dir).unwrap();
    let p = decode(
        &input::hex(include_str!("../examples/sum.hex")).unwrap(),
        0x1000,
        None,
    )
    .unwrap();
    let mut c = emit::c(&p).unwrap();
    c.push_str(
        r#"
#include <assert.h>
#include <string.h>
static unsigned char mem[512];
static uint64_t load(void *ctx, uint64_t addr, unsigned n) {
    (void)ctx; assert(addr <= sizeof(mem)-n); uint64_t v=0;
    for (unsigned j=0;j<n;j++) v |= (uint64_t)mem[addr+j] << (8*j);
    return v;
}
static void store(void *ctx, uint64_t addr, unsigned n, uint64_t v) {
    (void)ctx; assert(addr <= sizeof(mem)-n);
    for(unsigned j=0;j<n;j++) mem[addr+j]=(unsigned char)(v>>(8*j));
}
int main(void) {
    rosemary_machine m={0}; m.load=load; m.store=store; m.rip=ROSEMARY_ENTRY;
    m.r[4]=256; store(0,256,8,0xdead); m.r[0]=UINT64_MAX;
    assert(rosemary_run(&m,2)==1 && m.r[0]==0);
    assert(rosemary_run(&m,100)==0 && m.r[0]==15 && m.r[1]==0);
    assert(m.rip==0xdead && m.r[4]==264 && m.zf);
    rosemary_flags(&m,0x7fffffff,1,0x80000000,32,1);
    assert(m.of && !m.cf && m.sf && !m.zf && m.pf);
    rosemary_flags(&m,UINT64_MAX,1,0,64,1);
    assert(!m.of && m.cf && m.zf);
    rosemary_flags(&m,0,1,UINT64_MAX,64,2);
    assert(!m.of && m.cf && m.sf && !m.zf);
    rosemary_flags(&m,UINT64_C(0x8000000000000000),1,UINT64_C(0x7fffffffffffffff),64,2);
    assert(m.of && !m.cf && !m.sf);
    return 0;
}
"#,
    );
    fs::write(dir.join("loop.c"), c).unwrap();
    let compile = Command::new("cc")
        .args(["-std=c11", "-Wall", "-Wextra", "-Werror"])
        .arg(dir.join("loop.c"))
        .arg("-o")
        .arg(dir.join("loop"))
        .output()
        .expect("C compiler needed for generated C integration test");
    assert!(
        compile.status.success(),
        "{}",
        String::from_utf8_lossy(&compile.stderr)
    );
    assert!(Command::new(dir.join("loop")).status().unwrap().success());
    // call -> store [rsp-8], callee loads/stores via SIB, pop, ret -> sentinel
    let bytes =
        input::hex("e8 01 00 00 00 c3 48 c7 c0 2a 00 00 00 50 48 8b 0c 24 48 83 04 24 01 5a c3")
            .unwrap();
    let p = decode(&bytes, 0x1000, None).unwrap();
    let mut c = emit::c(&p).unwrap();
    let helpers = fs::read_to_string(dir.join("loop.c")).unwrap();
    let helper_start = helpers.find("#include <assert.h>").unwrap();
    let helper_end = helpers.find("int main(void)").unwrap();
    c.push_str(&helpers[helper_start..helper_end]);
    c.push_str("int main(void) { rosemary_machine m={0}; m.load=load; m.store=store; m.rip=ROSEMARY_ENTRY; m.r[4]=256; store(0,256,8,0xdead); assert(rosemary_run(&m,100)==0); assert(m.r[0]==42 && m.r[1]==42 && m.r[2]==43 && m.r[4]==264 && m.rip==0xdead); return 0; }\n");
    fs::write(dir.join("memory.c"), c).unwrap();
    let compile = Command::new("cc")
        .args(["-std=c11", "-Wall", "-Wextra", "-Werror"])
        .arg(dir.join("memory.c"))
        .arg("-o")
        .arg(dir.join("memory"))
        .output()
        .unwrap();
    assert!(
        compile.status.success(),
        "{}",
        String::from_utf8_lossy(&compile.stderr)
    );
    assert!(Command::new(dir.join("memory")).status().unwrap().success());
    fs::remove_dir_all(dir).unwrap();
}
#[test]
fn imported_ir_has_no_dependency_on_machine_bytes() {
    let p = Program {
        entry: 42,
        instructions: vec![Instruction {
            address: 42,
            len: 1,
            op: "mov".into(),
            operands: vec![Operand::Reg(0, 32), Operand::Imm(123, 32)],
        }],
    };
    assert!(
        emit::c(&Program::from_text(&p.to_text().unwrap()).unwrap())
            .unwrap()
            .contains("UINT64_C(0x7b)")
    );
}

#[test]
fn cli_pipeline_and_assembly_compilation() {
    let dir = std::env::temp_dir().join(format!("rosemary-cli-{}", std::process::id()));
    fs::create_dir_all(&dir).unwrap();
    let bin = env!("CARGO_BIN_EXE_rosemary");
    let lifted = Command::new(bin)
        .args(["lift", "--hex", "b8 2a 00 00 00 c3", "--base", "0x1000"])
        .output()
        .unwrap();
    assert!(lifted.status.success());
    fs::write(dir.join("input.ir"), &lifted.stdout).unwrap();
    for (to, extension) in [("c", "c"), ("asm", "s")] {
        let output = Command::new(bin)
            .arg("emit")
            .arg(dir.join("input.ir"))
            .args(["--to", to])
            .output()
            .unwrap();
        assert!(output.status.success());
        fs::write(dir.join(format!("output.{extension}")), &output.stdout).unwrap();
        let compile = Command::new("cc")
            .args(["-Wall", "-Wextra", "-Werror", "-c"])
            .arg(dir.join(format!("output.{extension}")))
            .arg("-o")
            .arg(dir.join(format!("{to}.o")))
            .output()
            .unwrap();
        assert!(
            compile.status.success(),
            "{}",
            String::from_utf8_lossy(&compile.stderr)
        );
    }
    let error = Command::new(bin)
        .args(["lift", "--hex", "0f 05"])
        .output()
        .unwrap();
    assert!(!error.status.success());
    assert!(String::from_utf8_lossy(&error.stderr).contains("0x0: unsupported"));
    assert!(error.stdout.is_empty());
    fs::remove_dir_all(dir).unwrap();
}

#[test]
fn cli_recursive_analysis_stdin_and_error_output_preservation() {
    let bin = env!("CARGO_BIN_EXE_rosemary");
    let lifted = Command::new(bin)
        .args([
            "lift",
            "--hex",
            "eb 02 0f 05 c3",
            "--recursive",
            "--base",
            "0x1000",
        ])
        .output()
        .unwrap();
    assert!(lifted.status.success());
    for to in ["text", "json", "dot"] {
        let mut child = Command::new(bin)
            .args(["analyze", "-", "--to", to])
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .unwrap();
        child
            .stdin
            .take()
            .unwrap()
            .write_all(&lifted.stdout)
            .unwrap();
        let output = child.wait_with_output().unwrap();
        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
        let text = String::from_utf8(output.stdout).unwrap();
        assert!(text.contains(match to {
            "json" => "\"schema\": \"rosemary-analysis-1\"",
            "dot" => "b_1000 -> b_1004",
            _ => "blocks: 2 (2 reachable)",
        }));
    }
    let mut child = Command::new(bin)
        .args(["lift", "-", "--recursive"])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .spawn()
        .unwrap();
    child
        .stdin
        .take()
        .unwrap()
        .write_all(&[0xc3, 0xff])
        .unwrap();
    assert!(child.wait_with_output().unwrap().status.success());
    for args in [
        vec!["lift", "--hex", "c3", "--no-follow-calls"],
        vec![
            "lift",
            "--hex",
            "c3",
            "--recursive",
            "--max-instructions",
            "0",
        ],
        vec!["lift", "--hex", "c3", "--recursive", "--recursive"],
        vec!["analyze", "--recursive"],
    ] {
        let error = Command::new(bin).args(args).output().unwrap();
        assert!(!error.status.success());
        assert!(error.stdout.is_empty());
    }
    let path = std::env::temp_dir().join(format!("rosemary-preserve-{}.ir", std::process::id()));
    fs::write(&path, "existing output").unwrap();
    let error = Command::new(bin)
        .args(["lift", "--hex", "90 0f 05", "--recursive", "-o"])
        .arg(&path)
        .output()
        .unwrap();
    assert!(!error.status.success());
    assert_eq!(fs::read_to_string(&path).unwrap(), "existing output");
    fs::remove_file(path).unwrap();
}

#[test]
fn cli_elf_range_uses_selected_start_and_base_override_does_not_overflow() {
    let bin = env!("CARGO_BIN_EXE_rosemary");
    let path = std::env::temp_dir().join(format!("rosemary-range-{}.elf", std::process::id()));
    let mut elf = elf_fixture();
    elf[24..32].copy_from_slice(&0x401005u64.to_le_bytes());
    fs::write(&path, &elf).unwrap();
    for (flags, expected) in [(vec![], 0x401005), (vec!["--size", "5"], 0x401000)] {
        let output = Command::new(bin)
            .arg("lift")
            .arg(&path)
            .args(flags)
            .output()
            .unwrap();
        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
        assert_eq!(
            Program::from_text(std::str::from_utf8(&output.stdout).unwrap())
                .unwrap()
                .entry,
            expected
        );
    }
    elf[208..216].copy_from_slice(&u64::MAX.to_le_bytes());
    elf[0x200..0x206].fill(0x90);
    fs::write(&path, &elf).unwrap();
    let output = Command::new(bin)
        .arg("lift")
        .arg(&path)
        .args(["--offset", "1", "--base", "0"])
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    fs::remove_file(path).unwrap();
}
