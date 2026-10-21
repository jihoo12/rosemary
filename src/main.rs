use rosemary::{Result, analysis, dataflow, decode, elf, emit, input, ir::Program, scan};
use std::{
    env, fs,
    io::{self, Read, Write},
    process,
};
const HELP: &str = "rosemary — dependency-free x86-64 reverse engineering

  rosemary lift FILE [--format raw|elf] [--base N] [--entry N]
                     [--offset N] [--size N] [--recursive]
                     [--no-follow-calls] [--max-instructions N] [-o FILE]
  rosemary lift ELF_FILE --function NAME [--linear] [-o FILE]
  rosemary lift ELF_FILE --section NAME [--linear] [-o FILE]
  rosemary lift ELF_FILE --all-code [--entry N] [--max-instructions N] [-o FILE]
  rosemary lift --hex 'b8 2a 00 00 00 c3' [--base N] [-o FILE]
  rosemary emit IR_FILE --to c|asm [-o FILE]
  rosemary analyze IR_FILE [--to text|json|dot|explain|summary] [-o FILE]
  rosemary symbols ELF_FILE [--to text|json] [-o FILE]
  rosemary sections ELF_FILE [--to text|json] [-o FILE]
  rosemary simplify IR_FILE [-o FILE]
  rosemary scan ELF_FILE [--filter TEXT] [--linear] [--max-instructions N] [--to text|json] [-o FILE]

Use - as the input file for stdin. Output defaults to stdout.
Lift into rosemary-ir v1; emit and analyze read that IR independently.
Input format defaults to auto-detection. Numbers are decimal or 0x hex.
ELF mode reads .text. Offset/size select a byte range within the input code.
Base overrides its virtual start address; entry must be an instruction boundary.
Recursive mode follows reachable paths, skipping data and unreachable code.
It follows direct calls by default; indirect targets are never guessed.
Function selection uses ELF symbols and implies recursive decoding of its range.
Explain infers constants and branches; simplify preserves addresses and lengths.
Unsupported reachable instructions fail explicitly. No external disassembler.
";
fn number(s: &str) -> Result<u64> {
    if let Some(s) = s.strip_prefix("0x") {
        u64::from_str_radix(s, 16)
    } else {
        s.parse()
    }
    .map_err(|_| format!("invalid number: {s}"))
}
fn run() -> Result<()> {
    let mut args = env::args().skip(1);
    let Some(command) = args.next() else {
        print!("{HELP}");
        return Ok(());
    };
    if matches!(command.as_str(), "--help" | "-h" | "help") {
        print!("{HELP}");
        return Ok(());
    }
    if !matches!(
        command.as_str(),
        "lift" | "emit" | "analyze" | "symbols" | "sections" | "simplify" | "scan"
    ) {
        return Err(format!("unknown command: {command}"));
    }
    let mut file = None;
    let mut output = None;
    let mut format = None;
    let mut base = None;
    let mut entry = None;
    let mut offset = None;
    let mut size = None;
    let mut hex = None;
    let mut to = None;
    let mut recursive = false;
    let mut linear = false;
    let mut all_code = false;
    let mut no_follow_calls = false;
    let mut max_instructions = None;
    let mut function = None;
    let mut section = None;
    let mut filter = None;
    while let Some(a) = args.next() {
        if a == "--help" || a == "-h" {
            print!("{HELP}");
            return Ok(());
        }
        if matches!(
            a.as_str(),
            "--recursive" | "--no-follow-calls" | "--linear" | "--all-code"
        ) {
            let flag = match a.as_str() {
                "--recursive" => &mut recursive,
                "--linear" => &mut linear,
                "--all-code" => &mut all_code,
                _ => &mut no_follow_calls,
            };
            if *flag {
                return Err(format!("duplicate option: {a}"));
            }
            *flag = true;
            continue;
        }
        if !a.starts_with('-') || a == "-" {
            if file.replace(a).is_some() {
                return Err("only one input file is allowed".into());
            }
            continue;
        }
        let slot = match a.as_str() {
            "-o" | "--output" => &mut output,
            "--format" => &mut format,
            "--base" => &mut base,
            "--entry" => &mut entry,
            "--offset" => &mut offset,
            "--size" => &mut size,
            "--hex" => &mut hex,
            "--to" => &mut to,
            "--max-instructions" => &mut max_instructions,
            "--function" => &mut function,
            "--section" => &mut section,
            "--filter" => &mut filter,
            _ => return Err(format!("unknown option: {a}")),
        };
        if slot.is_some() {
            return Err(format!("duplicate option: {a}"));
        }
        *slot = Some(
            args.next()
                .ok_or_else(|| format!("missing value for {a}"))?,
        );
    }
    let read = |f: &str| -> Result<Vec<u8>> {
        if f == "-" {
            let mut bytes = Vec::new();
            io::stdin()
                .lock()
                .read_to_end(&mut bytes)
                .map_err(|e| format!("stdin: {e}"))?;
            Ok(bytes)
        } else {
            fs::read(f).map_err(|e| format!("{f}: {e}"))
        }
    };
    let linear = linear || all_code;
    let recursive = recursive || (function.is_some() && !linear);
    if function.is_some() && section.is_some() {
        return Err("choose --function or --section".into());
    }
    if recursive && linear {
        return Err("choose --linear or --recursive".into());
    }
    if filter.is_some() && command != "scan" {
        return Err("--filter is only valid for scan".into());
    }
    let result = if command == "lift" {
        if to.is_some() {
            return Err("lift emits IR; --to is for emit, analyze, symbols or scan".into());
        }
        if !recursive && (no_follow_calls || (max_instructions.is_some() && !linear)) {
            return Err("--no-follow-calls and --max-instructions require --recursive".into());
        }
        if file.is_some() && hex.is_some() {
            return Err("choose a file or --hex".into());
        }
        if all_code
            && (function.is_some()
                || section.is_some()
                || base.is_some()
                || offset.is_some()
                || size.is_some()
                || hex.is_some())
        {
            return Err("--all-code cannot be combined with function, section, base, range or hex overrides".into());
        }
        if function.is_some()
            && (base.is_some()
                || entry.is_some()
                || offset.is_some()
                || size.is_some()
                || hex.is_some())
        {
            return Err(
                "--function cannot be combined with --hex, --base, --entry, --offset or --size"
                    .into(),
            );
        }
        let bytes = if let Some(h) = &hex {
            input::hex(h)?
        } else {
            read(file.as_deref().ok_or("missing input file or --hex")?)?
        };
        let format = format
            .as_deref()
            .unwrap_or(if bytes.starts_with(b"\x7fELF") {
                "elf"
            } else {
                "raw"
            });
        if all_code {
            if format != "elf" {
                return Err("--all-code requires ELF input".into());
            }
            let limit = max_instructions
                .as_deref()
                .map(number)
                .transpose()?
                .unwrap_or(1_000_000);
            decode::decode_elf(
                &bytes,
                entry.as_deref().map(number).transpose()?,
                usize::try_from(limit).map_err(|_| "instruction limit too large")?,
            )?
            .to_text()?
        } else {
            let code = match format {
                "raw" => input::Code {
                    bytes: &bytes,
                    base: 0,
                    entry: 0,
                },
                "elf" => {
                    if let Some(name) = &function {
                        input::elf_function(&bytes, name)?
                    } else if let Some(name) = &section {
                        input::elf_section(&bytes, name)?
                    } else {
                        input::elf_text(&bytes)?
                    }
                }
                _ => return Err("--format must be raw or elf".into()),
            };
            if (function.is_some() || section.is_some()) && format != "elf" {
                return Err("--function and --section require ELF input".into());
            }
            let restricted = offset.is_some() || size.is_some();
            let offset = offset.as_deref().map(number).transpose()?.unwrap_or(0);
            let off = usize::try_from(offset).map_err(|_| "offset too large")?;
            let rest = code.bytes.get(off..).ok_or("offset outside input code")?;
            let size = size
                .as_deref()
                .map(number)
                .transpose()?
                .unwrap_or(rest.len() as u64);
            let code_bytes = rest
                .get(..usize::try_from(size).map_err(|_| "size too large")?)
                .ok_or("size outside input code")?;
            let address = match base.as_deref() {
                Some(base) => number(base)?,
                None => code.base.checked_add(offset).ok_or("base overflow")?,
            };
            let entry = entry.as_deref().map(number).transpose()?.unwrap_or(
                if format == "elf" && base.is_none() && !restricted {
                    code.entry
                } else {
                    address
                },
            );
            let program = if recursive || linear {
                let mut options = decode::RecursiveOptions {
                    follow_calls: !no_follow_calls,
                    ..Default::default()
                };
                if let Some(limit) = max_instructions {
                    options.max_instructions = usize::try_from(number(&limit)?)
                        .map_err(|_| "instruction limit too large")?;
                    if options.max_instructions == 0 {
                        return Err("instruction limit must be positive".into());
                    }
                }
                if linear {
                    decode::decode_linear(
                        code_bytes,
                        address,
                        Some(entry),
                        options.max_instructions,
                    )?
                } else {
                    decode::decode_recursive(code_bytes, address, entry, options)?
                }
            } else {
                decode::decode(code_bytes, address, Some(entry))?
            };
            program.to_text()?
        }
    } else {
        if format.is_some()
            || base.is_some()
            || entry.is_some()
            || offset.is_some()
            || size.is_some()
            || hex.is_some()
            || recursive
            || (linear && command != "scan")
            || no_follow_calls
            || (max_instructions.is_some() && command != "scan")
            || function.is_some()
            || section.is_some()
            || all_code
        {
            return Err(format!("lift options cannot be used with {command}"));
        }
        let bytes = read(file.as_deref().ok_or("missing input file")?)?;
        if command == "scan" {
            let limit = max_instructions
                .as_deref()
                .map(number)
                .transpose()?
                .unwrap_or(10_000);
            let report = scan::scan_with_mode(
                &bytes,
                filter.as_deref(),
                usize::try_from(limit).map_err(|_| "instruction limit too large")?,
                if linear {
                    scan::ScanMode::Linear
                } else {
                    scan::ScanMode::Recursive
                },
            )?;
            match to.as_deref().unwrap_or("text") {
                "text" => report.text(),
                "json" => report.json(),
                _ => return Err("scan requires --to text|json".into()),
            }
        } else if command == "sections" {
            let sections = input::elf_sections(&bytes)?;
            match to.as_deref().unwrap_or("text") {
                "text" => elf::sections_text(&sections),
                "json" => elf::sections_json(elf::elf_entry(&bytes)?, &sections),
                _ => return Err("sections requires --to text|json".into()),
            }
        } else if command == "symbols" {
            let symbols = input::elf_functions(&bytes)?;
            match to.as_deref().unwrap_or("text") {
                "text" => elf::symbols_text(&symbols),
                "json" => elf::symbols_json(&symbols),
                _ => return Err("symbols requires --to text|json".into()),
            }
        } else {
            let p =
                Program::from_text(std::str::from_utf8(&bytes).map_err(|_| "IR must be UTF-8")?)?;
            if command == "simplify" {
                if to.is_some() {
                    return Err("simplify emits IR; use emit to translate the result".into());
                }
                dataflow::analyze(&p)?.simplify()?.to_text()?
            } else if command == "analyze" {
                let analysis = analysis::analyze(&p)?;
                match to.as_deref().unwrap_or("text") {
                    "text" => analysis.text(),
                    "json" => analysis.json(),
                    "dot" => analysis.dot(),
                    "explain" => analysis.values.text(),
                    "summary" => analysis.summary(),
                    _ => return Err("analyze requires --to text|json|dot|explain".into()),
                }
            } else {
                match to.as_deref() {
                    Some("c") => emit::c(&p)?,
                    Some("asm") => emit::assembly(&p)?,
                    _ => return Err("emit requires --to c|asm".into()),
                }
            }
        }
    };
    if let Some(path) = output {
        fs::write(&path, result).map_err(|e| format!("{path}: {e}"))?;
    } else {
        io::stdout()
            .lock()
            .write_all(result.as_bytes())
            .map_err(|e| e.to_string())?;
    }
    Ok(())
}
fn main() {
    if let Err(e) = run() {
        eprintln!("rosemary: {e}");
        process::exit(1)
    }
}
