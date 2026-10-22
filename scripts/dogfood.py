#!/usr/bin/env python3
"""Reverse the real Rosemary executable, cover every executable byte, compile core C."""
import argparse
import json
from pathlib import Path
import re
import shutil
import subprocess

ROOT = Path(__file__).resolve().parents[1]


def run(args, **kwargs):
    return subprocess.run([str(a) for a in args], check=True, **kwargs)


def read_ir(path):
    rows = []
    for line in path.read_text().splitlines()[2:]:
        fields = line.split()
        if fields:
            rows.append((int(fields[0], 16), int(fields[1])))
    return rows


def is_core(name):
    owner = name.split("8rosemary", 1)[0]
    return (name.startswith("_RNv")
            and not any(crate in owner for crate in ("5alloc", "4core", "3std"))
            and re.search(r"8rosemary(?:2ir|6decode|3elf|4emit|8analysis|8dataflow|4scan|6vector|8floating|7integer|7strings|3run|4main)", name))


def reference_mnemonic(encoded, words):
    """Normalize GNU spelling without using it to decode the subject."""
    encoded = [int(byte, 16) for byte in encoded]
    prefixes = []
    for byte in encoded:
        if byte not in (0x66, 0x67, 0x2e, 0x3e, 0x26, 0x36, 0xf2, 0xf3, 0x64, 0x65, 0xf0) and not 0x40 <= byte <= 0x4f:
            break
        prefixes.append(byte)
    words = list(words)
    prefix = ""
    while words:
        word = words[0]
        if word in ("data16", "addr32", "cs", "ds", "es", "ss", "fs", "gs") or word.startswith("rex"):
            words.pop(0)
        elif word in ("lock", "rep", "repz", "notrack"):
            prefix = {"lock": "lock.", "notrack": "notrack."}.get(words.pop(0), "rep.")
        else:
            break
    if not words:
        raise ValueError("GNU objdump instruction has no mnemonic")
    # GNU prints NOTRACK even when FS/GS suppress recognition in 64-bit mode.
    if prefix == "notrack." and any(byte in (0x64, 0x65) for byte in prefixes):
        prefix = ""
    mnemonic = words[0]
    if (mnemonic == "xchg" and len(words) > 1
            and words[1] in ("ax,ax", "eax,eax", "rax,rax") and encoded[-1] == 0x90):
        mnemonic = "nop"
    mnemonic = {"movabs": "mov", "cdqe": "movsxd", "cwde": "movsx", "cbw": "movsx",
                "sal": "shl"}.get(mnemonic, mnemonic)
    # Identify string opcodes separately from the identically named scalar MOVSD.
    opcode = encoded[len(prefixes)] if len(prefixes) < len(encoded) else None
    if opcode in (0xa4, 0xa5) and mnemonic in ("movs", "movsb", "movsw", "movsd", "movsq"):
        mnemonic = "movs"
    elif opcode in (0xaa, 0xab) and mnemonic in ("stos", "stosb", "stosw", "stosd", "stosq"):
        mnemonic = "stos"
    return prefix + mnemonic


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--binary", type=Path, help="existing Rosemary executable; default: build release")
    parser.add_argument("--output", type=Path, default=ROOT / "target/dogfood")
    parser.add_argument("--filter", default="rosemary", help="symbol-name substring for the focused report")
    parser.add_argument("--samples", type=int, default=5)
    parser.add_argument("--verify-boundaries", action="store_true",
                        help="independently compare native decoder boundaries with GNU objdump")
    args = parser.parse_args()
    if args.samples < 1:
        parser.error("--samples must be positive")
    if args.binary is None:
        run(["cargo", "build", "--release"], cwd=ROOT)
        binary = ROOT / "target/release/rosemary"
    else:
        binary = args.binary.resolve()
    output = args.output.resolve()
    output.mkdir(parents=True, exist_ok=True)
    # Freeze the subject so later builds cannot invalidate saved addresses/artifacts.
    subject = output / "subject.elf"
    if subject.resolve() != binary.resolve():
        shutil.copyfile(binary, subject)
    run([binary, "scan", subject, "--linear", "--to", "json", "-o", output / "all-functions.json"])
    all_functions = json.loads((output / "all-functions.json").read_text())
    sized_failures = [f for f in all_functions["functions"]
                      if f["status"] != "lifted" and int(f["size"], 16)]
    if sized_failures:
        raise RuntimeError(f"{len(sized_failures)} sized functions failed; see all-functions.json")
    run([binary, "scan", subject, "--linear", "--filter", args.filter, "--to", "json",
         "-o", output / "report.json"])
    report = json.loads((output / "report.json").read_text())
    inventory = json.loads(run([binary, "sections", subject, "--to", "json"],
                               stdout=subprocess.PIPE, text=True).stdout)
    sections = [s for s in inventory["sections"] if s["executable"] and int(s["size"], 16)]
    (output / "sections.json").write_text(json.dumps(inventory, indent=2) + "\n")
    run([binary, "lift", subject, "--all-code", "-o", output / "rosemary.ir"])
    full_rows = read_ir(output / "rosemary.ir")
    expected_bytes = sum(int(s["size"], 16) for s in sections)
    decoded_bytes = sum(size for _, size in full_rows)
    if decoded_bytes != expected_bytes:
        raise RuntimeError(f"executable coverage mismatch: {decoded_bytes}/{expected_bytes}")
    # Verify each section forms a contiguous stream with no skipped bytes.
    for section in sections:
        start, size = int(section["address"], 16), int(section["size"], 16)
        cursor = start
        for address, length in full_rows:
            if start <= address < start + size:
                if address != cursor or address + length > start + size:
                    raise RuntimeError(f"noncontiguous recovery in {section['name']}")
                cursor = address + length
        if cursor != start + size:
            raise RuntimeError(f"uncovered bytes in {section['name']}")
    run([binary, "emit", output / "rosemary.ir", "--to", "asm", "-o", output / "rosemary.s"])
    run(["cc","-c",output / "rosemary.s","-o",output / "rosemary.o"])
    run([binary, "analyze", output / "rosemary.ir", "--to", "summary", "-o", output / "summary.txt"])

    boundary_verified = False
    if args.verify_boundaries:
        reference = run(["objdump", "-d", "-M", "intel", "--insn-width=16", subject],
                        stdout=subprocess.PIPE, text=True).stdout
        oracle = {}
        oracle_ops = {}
        for line in reference.splitlines():
            match = re.match(r"\s*([0-9a-f]+):\s+((?:[0-9a-f]{2}(?:\s+|$))+)", line)
            if match:
                address = int(match[1],16)
                oracle[address] = len(match[2].split())
                oracle_ops[address] = reference_mnemonic(match[2].split(), line[match.end():].split())
        if oracle != dict(full_rows):
            differences = [(hex(a), n, oracle.get(a)) for a, n in full_rows if oracle.get(a) != n]
            raise RuntimeError(f"native/reference boundary mismatch: {differences[:8]}")
        native_ops = {int(line.split()[0],16):line.split()[2]
                      for line in (output / "rosemary.ir").read_text().splitlines()[2:]}
        differences = [(hex(a),op,oracle_ops.get(a)) for a,op in native_ops.items()
                       if oracle_ops.get(a) != op]
        if differences:
            raise RuntimeError(f"native/reference mnemonic mismatch: {differences[:8]}")
        boundary_verified = True

    core = [f for f in report["functions"] if f["status"] == "lifted" and is_core(f["name"])]
    if not core:
        raise RuntimeError("no real core Rosemary functions recovered")
    critical_names = ("8rosemary3run", "8rosemary4main", "8rosemary6decode3one",
                      "8rosemary8dataflow8transfer")
    critical = []
    for key in critical_names:
        critical.extend(f for f in core if key in f["name"])
    chosen = (critical + sorted((f for f in core if f not in critical),
                               key=lambda f: int(f["size"], 16)))[:args.samples]
    samples = []
    for function in chosen:
        stem = "function_" + function["address"]
        ir = output / (stem + ".ir")
        # Compiler clones can share a mangled name at distinct addresses.
        run([binary, "lift", subject, "--function-address", function["address"], "--linear", "-o", ir])
        rows = read_ir(ir)
        if sum(n for _, n in rows) != int(function["size"], 16):
            raise RuntimeError(f"partial function: {function['name']}")
        for kind, extension in [("c", "c"), ("asm", "s")]:
            run([binary, "emit", ir, "--to", kind, "-o", output / (stem + "." + extension)])
        run(["cc", "-std=c11", "-Wall", "-Wextra", "-Werror", "-c",
             output / (stem + ".c"), "-o", output / (stem + ".o")])
        for kind in ["json", "explain", "dot"]:
            run([binary, "analyze", ir, "--to", kind, "-o", output / (stem + "." + kind)])
        analysis = json.loads((output / (stem + ".json")).read_text())
        assert analysis["entry"] == function["address"]
        assert analysis["instruction_count"] == len(rows)
        samples.append({"name": function["name"], "artifact_stem": stem, "compiled_c": True})
    (output / "samples.json").write_text(json.dumps(samples, indent=2) + "\n")
    coverage = {
        "schema": "rosemary-self-reversal-1",
        "fingerprint_fnv1a": all_functions["fingerprint_fnv1a"],
        "entry": inventory["entry"], "executable_sections": sections,
        "executable_bytes": expected_bytes, "decoded_bytes": decoded_bytes,
        "instruction_count": len(full_rows), "all_sized_functions_lifted": True,
        "sized_functions": sum(int(f["size"], 16) > 0 for f in all_functions["functions"]),
        "unsized_symbols": [f["name"] for f in all_functions["functions"] if not int(f["size"], 16)],
        "reference_boundaries_verified": boundary_verified, "samples": samples,
        "reference_mnemonics_verified": boundary_verified,
        "full_assembly_compiled": True,
    }
    (output / "coverage.json").write_text(json.dumps(coverage, indent=2) + "\n")
    review = [
        "# Rosemary self-reversal",
        "",
        f"Executable coverage: **{decoded_bytes:,}/{expected_bytes:,} bytes**, "
        f"**{len(full_rows):,} instructions** across **{len(sections)} sections**.",
        f"All **{coverage['sized_functions']} sized functions** lifted linearly. "
        f"The {len(coverage['unsized_symbols'])} unsized startup symbols are covered by their sections.",
        "",
        "[Coverage manifest](coverage.json) · [Frozen subject](subject.elf) · "
        "[Full IR](rosemary.ir) · [Full assembly](rosemary.s) · "
        "[Assembled object](rosemary.o) · [Summary](summary.txt) · "
        "[All functions](all-functions.json)",
        "",
        "## Compiled core recoveries",
        "",
        "| Role | IR | C | Explanation | Graph |",
        "| --- | --- | --- | --- | --- |",
    ]
    labels = dict(zip(critical_names, ("CLI run", "CLI main", "x86 decoder", "Value transfer")))
    for sample in samples:
        label = next((label for key, label in labels.items() if key in sample["name"]), "Core helper")
        stem = sample["artifact_stem"]
        review.append(f"| {label} | [IR]({stem}.ir) | [C]({stem}.c) | "
                      f"[Explanation]({stem}.explain) | [DOT]({stem}.dot) |")
    review += ["", "Generated C models guest machine state and memory callbacks; "
               "OS/library calls and unresolved indirect destinations remain explicit.",
               "", f"Input fingerprint (FNV-1a): {coverage['fingerprint_fnv1a']}.",
               "Every instruction boundary, length and mnemonic matched GNU objdump."
               if boundary_verified else "Independent boundary checking was not requested.",
               ""]
    (output / "README.md").write_text("\n".join(review))
    print(f"Executable coverage: {decoded_bytes}/{expected_bytes} bytes, {len(full_rows)} instructions,"
          f" {len(sections)} sections.")
    print(f"All {coverage['sized_functions']} sized functions lifted linearly;"
          f" {len(coverage['unsized_symbols'])} unsized symbols covered through their sections.")
    print(f"Compiled {len(samples)} recovered core functions as C. Artifacts: {output}")
    if boundary_verified:
        print("Every instruction boundary/length and mnemonic also matched GNU objdump (validation only).")


if __name__ == "__main__":
    main()
