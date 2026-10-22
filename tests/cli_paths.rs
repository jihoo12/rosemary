use rosemary::ir::Program;
use std::{fs, process::Command};

#[test]
fn end_of_options_allows_dash_prefixed_input_and_help_named_files() {
    let bin = env!("CARGO_BIN_EXE_rosemary");
    let dir = std::env::temp_dir().join(format!("rosemary-cli-dash-{}", std::process::id()));
    fs::create_dir_all(&dir).unwrap();
    fs::write(dir.join("-input.bin"), [0xb8, 42, 0, 0, 0, 0xc3]).unwrap();
    fs::write(dir.join("--help"), [0xc3]).unwrap();
    let output = Command::new(bin)
        .current_dir(&dir)
        .args(["lift", "-o", "-output.ir", "--", "-input.bin"])
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(output.stdout.is_empty());
    let p = Program::from_text(&fs::read_to_string(dir.join("-output.ir")).unwrap()).unwrap();
    assert_eq!(p.instructions.len(), 2);
    let output = Command::new(bin)
        .current_dir(&dir)
        .args(["emit", "--to", "asm", "-o", "-output.s", "--", "-output.ir"])
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(
        fs::read_to_string(dir.join("-output.s"))
            .unwrap()
            .contains("mov eax, 0x2a")
    );
    let output = Command::new(bin)
        .current_dir(&dir)
        .args(["lift", "--", "--help"])
        .output()
        .unwrap();
    assert!(output.status.success());
    assert!(
        std::str::from_utf8(&output.stdout)
            .unwrap()
            .starts_with("rosemary-ir 1")
    );
    for args in [
        vec!["lift", "--"],
        vec!["lift", "--", "--help", "-input.bin"],
    ] {
        let output = Command::new(bin)
            .current_dir(&dir)
            .args(args)
            .output()
            .unwrap();
        assert_eq!(output.status.code(), Some(1));
        assert!(output.stdout.is_empty());
    }
    fs::remove_dir_all(dir).unwrap();
}

#[test]
#[cfg(unix)]
fn non_utf8_input_and_output_paths_work_across_the_pipeline() {
    use std::{ffi::OsString, os::unix::ffi::OsStringExt};
    let bin = env!("CARGO_BIN_EXE_rosemary");
    let dir = std::env::temp_dir().join(format!("rosemary-cli-nonutf8-{}", std::process::id()));
    fs::create_dir_all(&dir).unwrap();
    let input = OsString::from_vec(b"-input-\xff.bin".to_vec());
    let ir = OsString::from_vec(b"output-\xfe.ir".to_vec());
    let c = OsString::from_vec(b"output-\xfd.c".to_vec());
    fs::write(dir.join(&input), [0xb8, 42, 0, 0, 0, 0xc3]).unwrap();
    let output = Command::new(bin)
        .current_dir(&dir)
        .args(["lift", "-o"])
        .arg(&ir)
        .arg("--")
        .arg(&input)
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert_eq!(
        Program::from_text(&fs::read_to_string(dir.join(&ir)).unwrap())
            .unwrap()
            .instructions
            .len(),
        2
    );
    let output = Command::new(bin)
        .current_dir(&dir)
        .args(["emit", "--to", "c", "-o"])
        .arg(&c)
        .arg(&ir)
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(
        fs::read_to_string(dir.join(&c))
            .unwrap()
            .contains("int rosemary_run")
    );
    fs::remove_dir_all(dir).unwrap();
}

#[test]
#[cfg(unix)]
fn non_utf8_commands_and_option_values_fail_without_panics() {
    use std::{ffi::OsString, os::unix::ffi::OsStringExt};
    let invalid = OsString::from_vec(vec![0xff]);
    let invalid_option = OsString::from_vec(vec![b'-', 0xff]);
    for args in [
        vec![invalid.clone()],
        vec!["lift".into(), invalid_option],
        vec!["lift".into(), "--hex".into(), invalid.clone()],
        vec!["lift".into(), "--format".into(), invalid],
    ] {
        let output = Command::new(env!("CARGO_BIN_EXE_rosemary"))
            .args(args)
            .output()
            .unwrap();
        assert_eq!(output.status.code(), Some(1));
        assert!(output.stdout.is_empty());
        let error = String::from_utf8_lossy(&output.stderr);
        assert!(
            error.contains("UTF-8") && !error.contains("panicked"),
            "{error}"
        );
    }
}
