use rosemary::input::{elf_function, elf_function_at, elf_functions, elf_text};

fn put(bytes: &mut [u8], offset: usize, width: usize, value: u64) {
    bytes[offset..offset + width].copy_from_slice(&value.to_le_bytes()[..width]);
}
fn fixture() -> Vec<u8> {
    let mut b = vec![0; 0x400];
    b[..7].copy_from_slice(b"\x7fELF\x02\x01\x01");
    for (offset, width, value) in [
        (16, 2, 2),
        (18, 2, 62),
        (20, 4, 1),
        (24, 8, 0x401000),
        (40, 8, 64),
        (52, 2, 64),
        (58, 2, 64),
        (60, 2, 6),
        (62, 2, 1),
    ] {
        put(&mut b, offset, width, value);
    }
    let names = b"\0.shstrtab\0.text\0.strtab\0.symtab\0.dynsym\0";
    b[0x200..0x200 + names.len()].copy_from_slice(names);
    b[0x240..0x24e].copy_from_slice(b"\0answer\0alias\0");
    for (index, name, kind, flags, address, offset, size, link, stride) in [
        (1, 1, 3, 0, 0, 0x200, names.len() as u64, 0, 0),
        (2, 11, 1, 6, 0x401000, 0x300, 6, 0, 0),
        (3, 17, 3, 0, 0, 0x240, 14, 0, 0),
        (4, 25, 2, 0, 0, 0x280, 72, 3, 24),
        (5, 33, 11, 0, 0, 0x320, 48, 3, 24),
    ] {
        let h = 64 + index * 64;
        for (field, width, value) in [
            (0, 4, name),
            (4, 4, kind),
            (8, 8, flags),
            (16, 8, address),
            (24, 8, offset),
            (32, 8, size),
            (40, 4, link),
            (56, 8, stride),
        ] {
            put(&mut b, h + field, width, value);
        }
    }
    for (offset, name, info) in [(0x298, 1, 0x12), (0x2b0, 8, 2), (0x338, 1, 0x12)] {
        put(&mut b, offset, 4, name);
        put(&mut b, offset + 4, 1, info);
        put(&mut b, offset + 6, 2, 2);
        put(&mut b, offset + 8, 8, 0x401000);
        put(&mut b, offset + 16, 8, 6);
    }
    b[0x300..0x306].copy_from_slice(&[0xb8, 42, 0, 0, 0, 0xc3]);
    b
}
#[test]
fn static_dynamic_symbols_deduplicate_and_select_exact_function_ranges() {
    let b = fixture();
    let symbols = elf_functions(&b).unwrap();
    assert_eq!(symbols.len(), 2);
    assert_eq!(symbols[0].name, "alias");
    assert!(!symbols[0].dynamic);
    assert_eq!(symbols[1].name, "answer");
    assert!(symbols[1].dynamic);
    assert_eq!(symbols[1].address, 0x401000);
    assert_eq!(symbols[1].size, 6);
    let code = elf_function(&b, "answer").unwrap();
    assert_eq!(code.bytes, [0xb8, 42, 0, 0, 0, 0xc3]);
    assert_eq!(code.base, code.entry);
    assert!(
        elf_function(&b, "missing")
            .unwrap_err_text()
            .contains("not found")
    );
}
trait ErrorText {
    fn unwrap_err_text(self) -> String;
}
impl<T> ErrorText for rosemary::Result<T> {
    fn unwrap_err_text(self) -> String {
        match self {
            Ok(_) => panic!("expected error"),
            Err(e) => e,
        }
    }
}
#[test]
fn symbol_selection_works_in_executable_sections_other_than_text() {
    let mut b = fixture();
    b[0x20b..0x210].copy_from_slice(b".code");
    assert!(elf_text(&b).is_err());
    assert_eq!(elf_function(&b, "answer").unwrap().bytes.len(), 6);
    assert_eq!(elf_functions(&b).unwrap()[0].section, ".code");
}
#[test]
fn malformed_symbol_tables_and_truncations_are_rejected_without_panics() {
    let b = fixture();
    for n in 0..0x350 {
        assert!(elf_functions(&b[..n]).is_err(), "truncation {n:x}");
    }
    for (offset, width, value) in [
        (64 + 4 * 64 + 56, 8, 0),
        (64 + 4 * 64 + 56, 8, 23),
        (64 + 4 * 64 + 32, 8, 71),
        (64 + 4 * 64 + 40, 4, 99),
        (64 + 4 * 64 + 40, 4, 2),
        (0x298, 4, 99),
        (0x298 + 6, 2, 99),
        (0x298 + 6, 2, 0xffff),
        (0x298 + 8, 8, 0x400fff),
        (0x298 + 16, 8, 7),
        (64 + 3 * 64 + 32, 8, 7),
        (64 + 4 * 64 + 24, 8, u64::MAX),
    ] {
        let mut bad = b.clone();
        put(&mut bad, offset, width, value);
        assert!(
            elf_functions(&bad).is_err(),
            "offset {offset:x} value {value:x}"
        );
    }
    let mut bad = b;
    bad[0x241] = 0xff;
    assert!(elf_functions(&bad).is_err());
}
#[test]
fn zero_sized_undefined_and_ambiguous_functions_are_handled_explicitly() {
    let mut b = fixture();
    put(&mut b, 0x298 + 16, 8, 0);
    put(&mut b, 0x338 + 16, 8, 0);
    assert_eq!(elf_functions(&b).unwrap().len(), 2);
    assert!(
        elf_function(&b, "answer")
            .unwrap_err_text()
            .contains("no size")
    );
    let mut b = fixture();
    put(&mut b, 0x298 + 6, 2, 0);
    put(&mut b, 0x338 + 6, 2, 0);
    assert_eq!(elf_functions(&b).unwrap().len(), 1);
    let mut b = fixture();
    put(&mut b, 0x2b0, 4, 1);
    put(&mut b, 0x2b0 + 8, 8, 0x401001);
    put(&mut b, 0x2b0 + 16, 8, 5);
    assert!(
        elf_function(&b, "answer")
            .unwrap_err_text()
            .contains("ambiguous")
    );
}

#[test]
fn address_selection_distinguishes_duplicate_names_and_accepts_matching_aliases() {
    let b = fixture();
    assert_eq!(elf_function_at(&b, 0x401000).unwrap().bytes.len(), 6);
    assert!(
        elf_function_at(&b, 0x401001)
            .unwrap_err_text()
            .contains("no function entry")
    );
    let mut duplicate = b.clone();
    put(&mut duplicate, 0x2b0, 4, 1);
    put(&mut duplicate, 0x2b0 + 8, 8, 0x401005);
    put(&mut duplicate, 0x2b0 + 16, 8, 1);
    assert!(elf_function(&duplicate, "answer").is_err());
    let code = elf_function_at(&duplicate, 0x401005).unwrap();
    assert_eq!(code.bytes, [0xc3]);
    assert_eq!(code.entry, 0x401005);
    assert_eq!(code.base, code.entry);
    let mut unsized_alias = b.clone();
    put(&mut unsized_alias, 0x2b0 + 16, 8, 0);
    assert_eq!(
        elf_function_at(&unsized_alias, 0x401000)
            .unwrap()
            .bytes
            .len(),
        6
    );
    let mut conflicting = b;
    put(&mut conflicting, 0x2b0 + 16, 8, 5);
    assert!(
        elf_function_at(&conflicting, 0x401000)
            .unwrap_err_text()
            .contains("ambiguous function range")
    );
    let mut unsized_only = fixture();
    for offset in [0x298, 0x2b0, 0x338] {
        put(&mut unsized_only, offset + 16, 8, 0);
    }
    assert!(
        elf_function_at(&unsized_only, 0x401000)
            .unwrap_err_text()
            .contains("has no size")
    );
}

#[test]
fn cli_selects_function_addresses_and_rejects_conflicting_selectors() {
    use std::{fs, process::Command};
    let bin = env!("CARGO_BIN_EXE_rosemary");
    let path = std::env::temp_dir().join(format!(
        "rosemary-function-address-{}.elf",
        std::process::id()
    ));
    fs::write(&path, fixture()).unwrap();
    let output = Command::new(bin)
        .arg("lift")
        .arg(&path)
        .args(["--function-address", "0x401000"])
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let p = rosemary::ir::Program::from_text(std::str::from_utf8(&output.stdout).unwrap()).unwrap();
    assert_eq!(p.entry, 0x401000);
    assert_eq!(p.instructions.len(), 2);
    for extra in [
        vec!["--function", "answer"],
        vec!["--section", ".text"],
        vec!["--all-code"],
        vec!["--base", "0"],
        vec!["--entry", "0x401000"],
        vec!["--offset", "0"],
        vec!["--size", "6"],
        vec!["--format", "raw"],
        vec!["--hex", "c3"],
    ] {
        let output = Command::new(bin)
            .arg("lift")
            .arg(&path)
            .args(["--function-address", "0x401000"])
            .args(extra)
            .output()
            .unwrap();
        assert!(!output.status.success());
        assert!(output.stdout.is_empty());
    }
    fs::remove_file(path).unwrap();
}

#[test]
fn executable_section_selection_and_whole_image_recovery_validate_boundaries() {
    use rosemary::{decode, elf};
    let bytes = fixture();
    let sections = elf::elf_sections(&bytes).unwrap();
    let text = sections.iter().find(|s| s.name == ".text").unwrap();
    assert!(text.executable);
    assert_eq!(text.offset, 0x300);
    assert_eq!(text.size, 6);
    assert_eq!(
        elf::elf_section(&bytes, ".text").unwrap().bytes,
        [0xb8, 42, 0, 0, 0, 0xc3]
    );
    assert!(elf::elf_section(&bytes, ".strtab").is_err());
    assert!(elf::elf_section(&bytes, "missing").is_err());
    let p = decode::decode_elf(&bytes, None, 100).unwrap();
    assert_eq!(p.instructions.len(), 2);
    assert_eq!(p.entry, 0x401000);
    assert!(decode::decode_elf(&bytes, Some(0x401001), 100).is_err());
    assert!(decode::decode_elf(&bytes, None, 1).is_err());
    let mut overlapping = bytes;
    let header = 64 + 5 * 64;
    for (offset, width, value) in [
        (4, 4, 1),
        (8, 8, 4),
        (16, 8, 0x401000),
        (24, 8, 0x300),
        (32, 8, 6),
    ] {
        put(&mut overlapping, header + offset, width, value);
    }
    assert!(decode::decode_elf(&overlapping, None, 100).is_err());
}
