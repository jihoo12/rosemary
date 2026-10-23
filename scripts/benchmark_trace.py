#!/usr/bin/env python3
"""Measure bounded concrete memory traces using a prebuilt release binary."""
import argparse
import json
from pathlib import Path
import statistics
import subprocess
import tempfile
import time

ROOT = Path(__file__).resolve().parents[1]
STORE = """rosemary-ir 1
entry 1000
1000 1 add m:64:6:-:1:0 i:64:1
1001 1 add r:64:6 i:64:8
1002 1 loop i:64:1000
1003 1 ret
"""


def positive(value):
    value = int(value)
    if value < 1:
        raise argparse.ArgumentTypeError("must be positive")
    return value


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--binary", type=Path, default=ROOT / "target/release/rosemary")
    parser.add_argument("--iterations", type=positive, default=1_000_000)
    parser.add_argument("--repeats", type=positive, default=5)
    args = parser.parse_args()
    if args.iterations * 8 > 64 * 1024 * 1024 - 65536:
        parser.error("iterations exceed the CLI guest memory limit")
    binary = args.binary.resolve()
    with tempfile.TemporaryDirectory(prefix="rosemary-benchmark-") as directory:
        directory = Path(directory)
        store = directory / "store.ir"
        store.write_text(STORE)
        output = directory / "report.json"
        for name, source, stores, steps in [
            ("load/sum", ROOT / "examples/memory_sum.ir", 0, 3 * args.iterations + 3),
            ("read/modify/write", store, args.iterations, 3 * args.iterations + 1),
        ]:
            command = [str(binary), "trace", str(source), "--reg", f"rcx={args.iterations}",
                       "--reg", "rsi=0x2000", "--map", f"0x2000:{args.iterations * 8}",
                       "--max-steps", str(steps), "--trace-limit", "0", "--to", "json",
                       "-o", str(output)]
            timings = []
            # Warm up once; validate every run so early stops cannot look fast.
            for trial in range(args.repeats + 1):
                start = time.perf_counter()
                subprocess.run(command, check=True)
                elapsed = time.perf_counter() - start
                report = json.loads(output.read_text())
                if (report["stop"] != "left-code" or report["steps"] != steps
                        or report["memory_reads"] != args.iterations + 1
                        or report["memory_writes"] != stores
                        or report["final"]["registers"]["rsi"] != hex(0x2000 + args.iterations * 8)):
                    raise RuntimeError(f"unexpected {name} execution: {report}")
                if trial:
                    timings.append(elapsed)
            print(f"{name}: {args.iterations:,} iterations; median {statistics.median(timings):.4f}s; "
                  f"best {min(timings):.4f}s ({args.repeats} runs, one warmup)")


if __name__ == "__main__":
    main()
