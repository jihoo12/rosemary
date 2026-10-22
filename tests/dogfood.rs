//! Analyze the actual Cargo-built Rosemary executable, without a canary function.
use rosemary::{elf, emit, ir::Program, scan};
use std::{fs, process::Command};

#[test]
fn rosemary_reverses_its_own_functions_and_emits_compilable_c() {
    let bin = env!("CARGO_BIN_EXE_rosemary");
    let bytes = fs::read(bin).unwrap();
    let report = scan::scan(&bytes, Some("rosemary"), 2_000).unwrap();
    assert!(!report.functions.is_empty());
    assert!(report.lifted() >= 3, "{}", report.text());
    let core = report
        .functions
        .iter()
        .find(|f| {
            f.error.is_none()
                && f.symbol.name.starts_with("_RNv")
                && !["5alloc", "4core", "3std"].iter().any(|name| {
                    f.symbol
                        .name
                        .split("8rosemary")
                        .next()
                        .unwrap()
                        .contains(name)
                })
                && [
                    "8rosemary2ir",
                    "8rosemary6decode",
                    "8rosemary3elf",
                    "8rosemary4emit",
                    "8rosemary8dataflow",
                    "8rosemary8analysis",
                    "8rosemary4scan",
                ]
                .iter()
                .any(|s| f.symbol.name.contains(s))
        })
        .expect("at least one real core Rosemary function must lift, besides runtime/glue");
    let lifted = Command::new(bin)
        .args([
            "lift",
            bin,
            "--function-address",
            &format!("0x{:x}", core.symbol.address),
            "--no-follow-calls",
        ])
        .output()
        .unwrap();
    assert!(
        lifted.status.success(),
        "{}",
        String::from_utf8_lossy(&lifted.stderr)
    );
    let p = Program::from_text(std::str::from_utf8(&lifted.stdout).unwrap()).unwrap();
    assert_eq!(p.entry, core.symbol.address);
    let dir = std::env::temp_dir().join(format!("rosemary-dogfood-{}", std::process::id()));
    fs::create_dir_all(&dir).unwrap();
    fs::write(dir.join("self.c"), emit::c(&p).unwrap()).unwrap();
    let compile = Command::new("cc")
        .args(["-std=c11", "-Wall", "-Wextra", "-Werror", "-c"])
        .arg(dir.join("self.c"))
        .arg("-o")
        .arg(dir.join("self.o"))
        .output()
        .unwrap();
    assert!(
        compile.status.success(),
        "{}",
        String::from_utf8_lossy(&compile.stderr)
    );
    for to in ["json", "explain", "dot"] {
        fs::write(dir.join("self.ir"), p.to_text().unwrap()).unwrap();
        let output = Command::new(bin)
            .arg("analyze")
            .arg(dir.join("self.ir"))
            .args(["--to", to])
            .output()
            .unwrap();
        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
    }
    let output = Command::new(bin)
        .args([
            "scan",
            bin,
            "--filter",
            "rosemary",
            "--max-instructions",
            "2000",
            "--to",
            "json",
        ])
        .output()
        .unwrap();
    assert!(output.status.success());
    assert!(
        String::from_utf8(output.stdout)
            .unwrap()
            .contains("\"schema\": \"rosemary-scan-1\"")
    );
    assert!(!elf::elf_functions(&bytes).unwrap().is_empty());
    fs::remove_dir_all(dir).unwrap();
}
#[test]
fn scan_filters_and_limits_are_explicit() {
    let bytes = fs::read(env!("CARGO_BIN_EXE_rosemary")).unwrap();
    assert!(scan::scan(&bytes, None, 0).is_err());
    assert!(scan::scan(b"invalid", None, 100).is_err());
    let empty = scan::scan(&bytes, Some("definitely_no_such_function_42"), 1).unwrap();
    assert_eq!(empty.functions.len(), 0);
    assert_eq!(empty.lifted(), 0);
    let limited = scan::scan(&bytes, Some("rosemary"), 1).unwrap();
    assert!(
        limited
            .functions
            .iter()
            .any(|f| f.error.as_ref().is_some_and(|e| e.contains("limit")))
    );
}

#[test]
fn every_executable_byte_and_sized_function_of_rosemary_is_recovered() {
    use rosemary::{decode, scan::ScanMode};
    let binary = env!("CARGO_BIN_EXE_rosemary");
    let bytes = fs::read(binary).unwrap();
    let sections = elf::elf_sections(&bytes).unwrap();
    let program = decode::decode_elf(&bytes, None, 1_000_000).unwrap();
    let expected: usize = sections
        .iter()
        .filter(|s| s.executable)
        .map(|s| s.size as usize)
        .sum();
    assert_eq!(
        program
            .instructions
            .iter()
            .map(|i| i.len as usize)
            .sum::<usize>(),
        expected
    );
    assert_eq!(program.entry, elf::elf_entry(&bytes).unwrap());
    assert_eq!(
        Program::from_text(&program.to_text().unwrap()).unwrap(),
        program
    );
    let report = scan::scan_with_mode(&bytes, None, 1_000_000, ScanMode::Linear).unwrap();
    for function in &report.functions {
        if function.symbol.size != 0 {
            assert!(
                function.error.is_none(),
                "{}: {:?}",
                function.symbol.name,
                function.error
            );
            assert_eq!(function.decoded_bytes as u64, function.symbol.size);
        }
    }
    assert!(
        report
            .functions
            .iter()
            .any(|f| f.symbol.name.contains("8rosemary3run") && f.error.is_none())
    );
    assert!(
        report
            .functions
            .iter()
            .any(|f| f.symbol.name.contains("8rosemary4main") && f.error.is_none())
    );
    let out = std::env::temp_dir().join(format!("rosemary-complete-{}.ir", std::process::id()));
    let output = Command::new(binary)
        .args(["lift", binary, "--all-code", "-o"])
        .arg(&out)
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let decoded = Program::from_text(&fs::read_to_string(&out).unwrap()).unwrap();
    assert_eq!(decoded, program);
    fs::remove_file(out).unwrap();
}
