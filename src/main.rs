use rosemary::{Result, analysis, dataflow, decode, dynamic, elf, emit, input, ir::Program, scan};
use std::{
    env,
    ffi::OsStr,
    fs,
    io::{self, Read, Write},
    path::Path,
    process,
};
const HELP: &str = "rosemary — dependency-free x86-64 reverse engineering

  rosemary lift FILE [--format raw|elf] [--base N] [--entry N]
                     [--offset N] [--size N] [--linear|--recursive]
                     [--no-follow-calls] [--max-instructions N] [-o FILE]
  rosemary lift ELF_FILE --function NAME [--linear] [-o FILE]
  rosemary lift ELF_FILE --function-address N [--linear] [-o FILE]
  rosemary lift ELF_FILE --section NAME [--linear] [-o FILE]
  rosemary lift ELF_FILE --all-code [--entry N] [--max-instructions N] [-o FILE]
  rosemary lift --hex 'b8 2a 00 00 00 c3' [--base N] [-o FILE]
  rosemary emit IR_FILE --to c|asm [-o FILE]
  rosemary analyze IR_FILE [--to text|json|dot|explain|summary] [-o FILE]
  rosemary trace IR_FILE [--reg NAME=N] [--flag NAME=0|1]
                         [--memory ADDRESS:HEX] [--map ADDRESS:SIZE]
                         [--max-steps N] [--trace-limit N] [--to text|json] [-o FILE]
  rosemary symbols ELF_FILE [--to text|json] [-o FILE]
  rosemary sections ELF_FILE [--to text|json] [-o FILE]
  rosemary simplify IR_FILE [-o FILE]
  rosemary scan ELF_FILE [--filter TEXT] [--linear|--recursive] [--max-instructions N] [--to text|json] [-o FILE]

Use - as the input file for stdin. Output defaults to stdout.
Use -- before an input filename that starts with a dash.
Lift into rosemary-ir v1; emit and analyze read that IR independently.
Input format defaults to auto-detection. Numbers are decimal or 0x hex.
ELF mode reads .text. Offset/size select a byte range within the input code.
Base overrides its virtual start address; entry must be an instruction boundary.
Recursive mode follows reachable paths, skipping data and unreachable code.
Lift limits default to 1,000,000 instructions; scan limits to 10,000 per function.
It follows direct calls by default; indirect targets are never guessed.
Function selection uses ELF symbols and implies recursive decoding of its range.
Explain infers constants and branches; simplify preserves addresses and lengths.
Trace executes IR with concrete inputs, mapped guest memory and a bounded budget.
Trace defaults: zero registers/flags, 64 KiB stack, 10,000 steps, 1,000 trace rows.
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
fn instruction_limit(value: Option<&str>, default: usize) -> Result<usize> {
    let limit = value
        .map(number)
        .transpose()?
        .map(usize::try_from)
        .transpose()
        .map_err(|_| "instruction limit too large")?
        .unwrap_or(default);
    if limit == 0 {
        return Err("instruction limit must be positive".into());
    }
    Ok(limit)
}

fn trace_machine(entry: u64, settings: &[(String, String)]) -> Result<dynamic::Machine> {
    let mut m = dynamic::Machine::with_stack(entry);
    let mut mapped = 65536u64;
    const MAX_MEMORY: u64 = 64 * 1024 * 1024;
    for (option, value) in settings {
        if matches!(option.as_str(), "--reg" | "--flag") {
            let (name, value) = value.split_once('=').ok_or("expected NAME=VALUE")?;
            let value = number(value)?;
            if option == "--reg" {
                if let Some(index) = dynamic::REGISTERS.iter().position(|r| *r == name) {
                    m.state.registers[index] = value;
                } else {
                    match name {
                        "fs_base" => m.state.fs_base = value,
                        "gs_base" => m.state.gs_base = value,
                        _ => {
                            return Err(format!(
                                "unknown register: {name}; use a 64-bit register name"
                            ));
                        }
                    }
                }
            } else {
                if value > 1 {
                    return Err("flag values must be 0 or 1".into());
                }
                let flag = match name {
                    "zf" => &mut m.state.flags.zf,
                    "sf" => &mut m.state.flags.sf,
                    "cf" => &mut m.state.flags.cf,
                    "of" => &mut m.state.flags.of,
                    "pf" => &mut m.state.flags.pf,
                    "df" => &mut m.state.flags.df,
                    _ => return Err(format!("unknown flag: {name}")),
                };
                *flag = value != 0;
            }
        } else {
            let (address, data) = value
                .split_once(':')
                .ok_or("expected ADDRESS:HEX or ADDRESS:SIZE")?;
            let address = number(address)?;
            let bytes = if option == "--map" {
                let size = number(data)?;
                if size == 0 || size > MAX_MEMORY - mapped {
                    return Err(
                        "guest memory limit is 64 MiB; mapping size must be positive".into(),
                    );
                }
                vec![0; size as usize]
            } else {
                // Check before allocating the decoded byte vector.
                let digits = data
                    .bytes()
                    .filter(|b| !b.is_ascii_whitespace() && *b != b'_')
                    .count();
                if digits as u64 > (MAX_MEMORY - mapped) * 2 {
                    return Err("guest memory limit is 64 MiB".into());
                }
                input::hex(data)?
            };
            mapped += bytes.len() as u64;
            m.memory.map(address, bytes, true)?;
        }
    }
    Ok(m)
}

fn run() -> Result<()> {
    let mut args = env::args_os().skip(1);
    let Some(command) = args.next() else {
        print!("{HELP}");
        return Ok(());
    };
    let command = command.to_str().ok_or("command must be UTF-8")?;
    if matches!(command, "--help" | "-h" | "help") {
        print!("{HELP}");
        return Ok(());
    }
    if !matches!(
        command,
        "lift" | "emit" | "analyze" | "trace" | "symbols" | "sections" | "simplify" | "scan"
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
    let mut function_address = None;
    let mut section = None;
    let mut filter = None;
    let mut max_steps = None;
    let mut trace_limit = None;
    let mut trace_settings = Vec::new();
    let mut end_of_options = false;
    while let Some(a) = args.next() {
        if !end_of_options && a == "--" {
            end_of_options = true;
            continue;
        }
        if end_of_options || !a.as_encoded_bytes().starts_with(b"-") || a == "-" {
            if file.replace(a).is_some() {
                return Err("only one input file is allowed".into());
            }
            continue;
        }
        let a = a
            .to_str()
            .ok_or("option must be UTF-8; use -- before input filenames")?;
        if a == "--help" || a == "-h" {
            print!("{HELP}");
            return Ok(());
        }
        if matches!(
            a,
            "--recursive" | "--no-follow-calls" | "--linear" | "--all-code"
        ) {
            let flag = match a {
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
        if matches!(a, "-o" | "--output") {
            if output.is_some() {
                return Err(format!("duplicate option: {a}"));
            }
            output = Some(
                args.next()
                    .ok_or_else(|| format!("missing value for {a}"))?,
            );
            continue;
        }
        if matches!(a, "--reg" | "--flag" | "--memory" | "--map") {
            trace_settings.push((
                a.to_owned(),
                args.next()
                    .ok_or_else(|| format!("missing value for {a}"))?
                    .into_string()
                    .map_err(|_| format!("value for {a} must be UTF-8"))?,
            ));
            continue;
        }
        let slot = match a {
            "--format" => &mut format,
            "--base" => &mut base,
            "--entry" => &mut entry,
            "--offset" => &mut offset,
            "--size" => &mut size,
            "--hex" => &mut hex,
            "--to" => &mut to,
            "--max-instructions" => &mut max_instructions,
            "--function" => &mut function,
            "--function-address" => &mut function_address,
            "--section" => &mut section,
            "--filter" => &mut filter,
            "--max-steps" => &mut max_steps,
            "--trace-limit" => &mut trace_limit,
            _ => return Err(format!("unknown option: {a}")),
        };
        if slot.is_some() {
            return Err(format!("duplicate option: {a}"));
        }
        *slot = Some(
            args.next()
                .ok_or_else(|| format!("missing value for {a}"))?
                .into_string()
                .map_err(|_| format!("value for {a} must be UTF-8"))?,
        );
    }
    let read = |f: &OsStr| -> Result<Vec<u8>> {
        if f == OsStr::new("-") {
            let mut bytes = Vec::new();
            io::stdin()
                .lock()
                .read_to_end(&mut bytes)
                .map_err(|e| format!("stdin: {e}"))?;
            Ok(bytes)
        } else {
            let path = Path::new(f);
            fs::read(path).map_err(|e| format!("{}: {e}", path.display()))
        }
    };
    if function.is_some() && function_address.is_some() {
        return Err("choose --function or --function-address".into());
    }
    let function_selected = function.is_some() || function_address.is_some();
    let linear = linear || all_code;
    let recursive = recursive || (function_selected && !linear);
    if function_selected && section.is_some() {
        return Err("choose a function selector or --section".into());
    }
    if recursive && linear {
        return Err("choose --linear or --recursive".into());
    }
    if filter.is_some() && command != "scan" {
        return Err("--filter is only valid for scan".into());
    }
    if command != "trace"
        && (max_steps.is_some() || trace_limit.is_some() || !trace_settings.is_empty())
    {
        return Err("trace options are only valid for trace".into());
    }
    let result = if command == "lift" {
        if to.is_some() {
            return Err("lift emits IR; --to is for emit, analyze, symbols or scan".into());
        }
        if !recursive && no_follow_calls {
            return Err("--no-follow-calls requires --recursive".into());
        }
        if file.is_some() && hex.is_some() {
            return Err("choose a file or --hex".into());
        }
        if all_code
            && (function_selected
                || section.is_some()
                || base.is_some()
                || offset.is_some()
                || size.is_some()
                || hex.is_some())
        {
            return Err("--all-code cannot be combined with function, section, base, range or hex overrides".into());
        }
        if function_selected
            && (base.is_some()
                || entry.is_some()
                || offset.is_some()
                || size.is_some()
                || hex.is_some())
        {
            return Err(
                "function selectors cannot be combined with --hex, --base, --entry, --offset or --size"
                    .into(),
            );
        }
        let limit = instruction_limit(max_instructions.as_deref(), 1_000_000)?;
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
            decode::decode_elf(&bytes, entry.as_deref().map(number).transpose()?, limit)?
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
                    } else if let Some(address) = &function_address {
                        input::elf_function_at(&bytes, number(address)?)?
                    } else if let Some(name) = &section {
                        input::elf_section(&bytes, name)?
                    } else {
                        input::elf_text(&bytes)?
                    }
                }
                _ => return Err("--format must be raw or elf".into()),
            };
            if (function_selected || section.is_some()) && format != "elf" {
                return Err("function and section selectors require ELF input".into());
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
            let program = if recursive {
                decode::decode_recursive(
                    code_bytes,
                    address,
                    entry,
                    decode::RecursiveOptions {
                        follow_calls: !no_follow_calls,
                        max_instructions: limit,
                    },
                )?
            } else {
                decode::decode_linear(code_bytes, address, Some(entry), limit)?
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
            || (recursive && command != "scan")
            || (linear && command != "scan")
            || no_follow_calls
            || (max_instructions.is_some() && command != "scan")
            || function_selected
            || section.is_some()
            || all_code
        {
            return Err(format!("lift options cannot be used with {command}"));
        }
        let bytes = read(file.as_deref().ok_or("missing input file")?)?;
        if command == "scan" {
            let limit = instruction_limit(max_instructions.as_deref(), 10_000)?;
            let report = scan::scan_with_mode(
                &bytes,
                filter.as_deref(),
                limit,
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
            if command == "trace" {
                let options = dynamic::Options {
                    max_steps: max_steps
                        .as_deref()
                        .map(number)
                        .transpose()?
                        .unwrap_or(10_000),
                    trace_limit: trace_limit
                        .as_deref()
                        .map(number)
                        .transpose()?
                        .map(usize::try_from)
                        .transpose()
                        .map_err(|_| "trace limit too large")?
                        .unwrap_or(1_000),
                };
                let mut machine = trace_machine(p.entry, &trace_settings)?;
                let report = dynamic::execute(&p, &mut machine, options)?;
                match to.as_deref().unwrap_or("text") {
                    "text" => report.text(),
                    "json" => report.json(),
                    _ => return Err("trace requires --to text|json".into()),
                }
            } else if command == "simplify" {
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
        let path = Path::new(&path);
        fs::write(path, result).map_err(|e| format!("{}: {e}", path.display()))?;
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
