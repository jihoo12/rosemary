use rosemary::ir::Program;
use std::{
    fs,
    process::{Command, Output},
};

fn run(args: &[&str]) -> Output {
    Command::new(env!("CARGO_BIN_EXE_rosemary"))
        .args(args)
        .output()
        .unwrap()
}

#[test]
fn default_linear_lifting_accepts_and_enforces_instruction_limits() {
    for mode in [None, Some("--linear"), Some("--recursive")] {
        let mut args = vec![
            "lift",
            "--hex",
            "90 c3",
            "--base",
            "0x1000",
            "--max-instructions",
            "2",
        ];
        if let Some(mode) = mode {
            args.push(mode);
        }
        let output = run(&args);
        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
        let p = Program::from_text(std::str::from_utf8(&output.stdout).unwrap()).unwrap();
        assert_eq!(p.instructions.len(), 2);
        args[6] = "1";
        let output = run(&args);
        assert!(!output.status.success());
        assert!(output.stdout.is_empty());
        let error = String::from_utf8_lossy(&output.stderr);
        assert!(
            error.contains("0x1001:") && error.contains("limit (1) exceeded"),
            "{error}"
        );
    }
}

#[test]
fn invalid_limits_fail_consistently_in_every_decoding_mode() {
    let binary = env!("CARGO_BIN_EXE_rosemary");
    for options in [
        vec!["lift", "--hex", "c3"],
        vec!["lift", "--hex", "c3", "--linear"],
        vec!["lift", "--hex", "c3", "--recursive"],
        vec!["lift", binary, "--all-code"],
        vec!["scan", binary],
        vec!["scan", binary, "--linear"],
        vec!["scan", binary, "--recursive"],
    ] {
        for (value, message) in [
            ("0", "instruction limit must be positive"),
            ("invalid", "invalid number"),
        ] {
            let mut args = options.clone();
            args.extend(["--max-instructions", value]);
            let output = run(&args);
            assert!(!output.status.success());
            assert!(output.stdout.is_empty());
            assert!(
                String::from_utf8_lossy(&output.stderr).contains(message),
                "{args:?}: {}",
                String::from_utf8_lossy(&output.stderr)
            );
        }
    }
}

#[test]
fn scan_accepts_explicit_recursive_mode_and_rejects_conflicting_modes() {
    let binary = env!("CARGO_BIN_EXE_rosemary");
    let args = [
        "scan",
        binary,
        "--filter",
        "definitely_no_matching_function_1234",
        "--to",
        "json",
    ];
    let default = run(&args);
    let mut explicit = args.to_vec();
    explicit.push("--recursive");
    let recursive = run(&explicit);
    assert!(
        default.status.success() && recursive.status.success(),
        "{}",
        String::from_utf8_lossy(&recursive.stderr)
    );
    assert_eq!(default.stdout, recursive.stdout);
    for command in ["scan", "lift"] {
        let output = run(&[command, binary, "--linear", "--recursive"]);
        assert!(!output.status.success());
        assert!(String::from_utf8_lossy(&output.stderr).contains("choose --linear or --recursive"));
        assert!(output.stdout.is_empty());
    }
}

#[test]
fn failed_default_linear_limit_keeps_existing_output_intact() {
    let path =
        std::env::temp_dir().join(format!("rosemary-limit-output-{}.ir", std::process::id()));
    fs::write(&path, "existing output").unwrap();
    let output = Command::new(env!("CARGO_BIN_EXE_rosemary"))
        .args(["lift", "--hex", "90 c3", "--max-instructions", "1", "-o"])
        .arg(&path)
        .output()
        .unwrap();
    assert!(!output.status.success());
    assert!(String::from_utf8_lossy(&output.stderr).contains("linear instruction limit"));
    assert_eq!(fs::read_to_string(&path).unwrap(), "existing output");
    fs::remove_file(path).unwrap();
}
