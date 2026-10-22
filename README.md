# rosemary

A dependency-free reverse engineering tool written in Rust. Rosemary recovers
x86-64 instructions using linear or recursive decoding, analyzes control flow,
and translates its typed machine-level IR into executable C or Intel-syntax
assembly. It selects ELF functions by name, propagates constants and flags,
explains branch decisions, and simplifies proven control transfers. Analysis
includes basic blocks, call sites, reachability, function-entry candidates, and
loop components, with text, JSON, and Graphviz DOT output. A batch scanner and
self-reversing dogfood tests expose real compiler coverage and remaining gaps.

```text
Raw bytes / ELF64 code -> linear / recursive decoder -> rosemary-ir v1
                                                         |    |    |
                                                         C   ASM  CFG
                                                                   |
                                                           text / JSON / DOT
```

There are no third-party Rust crates. The tool does not invoke objdump, LLVM,
Capstone, a compiler, or any other external program at runtime.

## Build and run

Use Rust 1.88 or newer, with Cargo.

```sh
cargo build --release

# Lift a function that returns 42.
cargo run -- lift --hex 'b8 2a 00 00 00 c3' --base 0x1000 -o function.ir

# Each backend reads the saved IR independently.
cargo run -- emit function.ir --to c -o function.c
cargo run -- emit function.ir --to asm -o function.s

# Decode a raw binary, with an explicit virtual address.
cargo run -- lift code.bin --format raw --base 0x401000 -o code.ir

# ELF64 little-endian x86-64 executable or shared object: read .text.
cargo run -- lift program.elf --format elf -o program.ir

# Recover only reachable instructions, skipping embedded data and dead padding.
cargo run -- lift program.elf --recursive -o reachable.ir
cargo run -- analyze reachable.ir
cargo run -- analyze reachable.ir --to json -o analysis.json
cargo run -- analyze reachable.ir --to dot -o control-flow.dot

# Reverse the complete executable, including startup and PLT code.
cargo run -- sections program.elf --to json
cargo run -- lift program.elf --all-code -o complete.ir
cargo run -- analyze complete.ir --to summary
cargo run -- lift program.elf --section .init --linear -o init.ir

# Find and select a function.
cargo run -- symbols program.elf
cargo run -- lift program.elf --function factorial -o factorial.ir
cargo run -- analyze factorial.ir --to explain

# Select an exact symbol entry when compiler clones share the same name.
cargo run -- lift program.elf --function-address 0x401000 -o selected.ir

# Simplify confirmed control transfers, or scan an ELF function by function.
cargo run -- simplify examples/proven_branch.ir -o simplified.ir
cargo run -- scan program.elf --linear --to json -o coverage.json

# Restrict decoding to a known code range, relative to the selected input code.
cargo run -- lift code.bin --offset 0x20 --size 0x40 --base 0x401020 -o range.ir

cargo run -- --help
```

Input format defaults to ELF detection by magic bytes, otherwise raw bytes.
Numbers accept decimal or `0x` hexadecimal notation. `--hex` accepts whitespace
and underscores between hex digits; signs and other non-hex characters are rejected.
Output goes to stdout unless `-o` is given.
Errors go to stderr with a nonzero exit status; decoding finishes before output
is written, so an unsupported instruction does not produce partial IR.
Use `-` as the input filename to read bytes or IR from stdin, for example
`cargo run -- emit - --to c < function.ir`. All commands accept `-o FILE`.
Use `--` before an input filename that starts with a dash, for example
`rosemary lift -- -code.bin`. Input and output paths preserve the operating
system's filename encoding, including non-UTF-8 Unix paths. Commands and option
values such as function names and output formats must be UTF-8.

`--offset` and `--size` select bytes within the raw input or ELF `.text` section.
Without `--base`, the selected start address is the input base plus the offset.
An explicit `--base` sets the selected range's start address. The default entry
is the selected start, except when reading an entire ELF `.text` without a base
override: then the ELF entry is used, or `.text` start when ELF entry is zero.
Use `--entry` to override it. Entry must match a decoded instruction boundary.

`--section NAME` selects an executable PROGBITS section, keeping the ELF entry
when it lies inside that section, otherwise using the section start. Range/base
overrides still apply. `--all-code` linearly decodes **every executable PROGBITS
section**, combines their original addresses into one sorted IR, and preserves
the ELF entry (or uses the first code address when entry is zero). It never reads
gaps between sections as code. This mode rejects section/function/base/range/hex
overrides; `--entry` and `--max-instructions` remain available. Overlapping code,
unsupported bytes, or an invalid entry fail explicitly, without partial output.

## Supported machine code

The current decoder supports these x86-64 encodings:

- 8-, 16-, 32- and 64-bit `mov`, `add`, `sub`, `xor`, `and`, `or`, `cmp`, `test`,
  `inc`, `dec`, `neg`, `not`, and `shl`/`shr`/`sar` with immediate or CL counts.
- 16-, 32- and 64-bit `lea`, two/three-operand `imul`, and all integer `cmovcc`;
  all integer `setcc` to byte registers or memory.
- `movzx` and `movsx` from byte/word registers or memory into wider registers.
- `movsxd` from a 32-bit register or memory into a 64-bit register.
- `adc`/`sbb`, `clc`/`stc`/`cmc`, `xchg`, one-operand signed/unsigned multiply/divide,
  `cwd`/`cdq`/`cqo`, `rol`/`ror`/`rcl`/`rcr`, `shld`/`shrd`, `bswap`, `bt`/`bts`/`btr`/`btc`,
  `bsf`/`bsr`, `tzcnt`/`lzcnt`/`popcnt` at 16/32/64 bits, `cmpxchg` and `xadd`.
- Legacy high-byte registers AH/CH/DH/BH; FS/GS memory bases; valid LOCK prefixes
  on supported memory updates. LOCK is retained in IR/assembly; C models one
  sequential guest machine without atomic concurrency.
- `cld`/`std` and byte/word/dword/qword MOVS/STOS, with optional REP.
- SSE/SSE2 128-bit moves, scalar bit moves, logic, wrapping and saturating packed
  integer add/subtract, rounded unsigned averages, equality/signed comparisons,
  unsigned byte and signed word min/max,
  shuffles, unpacking, shifts,
  unsigned doubleword multiplication, insert/extract, and mask extraction.
  Supported families include MOVUPS/MOVAPS/MOVUPD/MOVAPD/MOVDQU/MOVDQA, MOVD/MOVQ,
  MOVSS/MOVSD, MOVLPS/MOVHPS/MOVLPD/MOVHPD memory loads/stores,
  PXOR/PAND/PANDN/POR and corresponding PS/PD logic,
  PADDB/W/D/Q, PSUBB/W/D/Q, PADDSB/W, PSUBSB/W, PADDUSB/W, PSUBUSB/W,
  PAVGB/W, PCMPEQB/W/D, PCMPGTB/W/D, PMINUB/PMAXUB,
  PMINSW/PMAXSW, PSHUFD/LW/HW,
  PUNPCKL/HBW/WD/DQ/QDQ, PSLL/PSRLW/D/Q, PSRAW/D, PSLLDQ/PSRLDQ, PMULUDQ,
  PINSRW/PEXTRW, PMOVMSKB, MOVMSKPS/PD and register MOVLHPS/MOVHLPS.
- PAUSE, UD2, INT3 and HLT. Explicit trap/stop instructions are retained and
  stop C execution at the current guest address.
- Register, immediate, and memory operands, including ModR/M, SIB, REX register
  extensions, signed displacements, and RIP-relative addressing.
- 64-bit `push`, `pop`, `call`, `ret` (including unsigned imm16 stack cleanup),
  `leave`, and direct/indirect `jmp`.
- All 16 integer condition-code branches, using short or near encodings.
- `loop`/`loope`/`loopne` with 64-bit RCX counters, plus `jrcxz`.
- Single-byte NOP and memory-form `0f 1f /0` multi-byte NOP, including `66` and
  `2e` prefixes used for compiler alignment. `endbr64` is retained as a no-op in C;
  CET enforcement is not modeled.

Support remains bounded: 16-bit stack operations, address-size overrides, other
segment behavior, AVX/AVX-512, floating-point arithmetic, syscalls, and many
other instructions/forms are not implemented. TZCNT/LZCNT/POPCNT are modeled with
their count semantics, independent of host CPU feature support. POPCNT clears
CF/OF/SF/PF and sets ZF when its source is zero; a zero result from TZCNT/LZCNT
instead sets ZF. Both `66 F3` and `F3 66` prefix orders select 16-bit count
operands, with REX.W selecting 64 bits. Unsupported or truncated
encodings fail at their original virtual address. No instruction length is
silently guessed.

Linear decoding remains the default and visits every byte in the selected range.
Embedded data or unsupported padding can stop a linear lift; use `--recursive`
or select a known range with `--offset` and `--size`. There is no relocation
processing, dynamic linking, PE/Mach-O reader, or support for other CPU
architectures. ELF input requires a normal section table; relocatable objects and
extended section indexes are rejected.

`symbols` reads defined ordinary function symbols from static and dynamic ELF
symbol tables, merges identical entries, and sorts them by address/name. It skips
undefined imports and nonexecutable sections. `--to json` emits
`rosemary-symbols-1`, including name, address, size, section and dynamic-table
presence. `lift --function NAME` requires an unambiguous symbol with a nonzero
size, preserves its virtual addresses, and implies recursive decoding bounded
to that symbol's executable section range, including sections outside `.text`.
Add `--linear` to decode every byte in that symbol, including code not discovered
through indirect jumps.
Calls outside that range remain external. The selector cannot be combined with
base/entry/range overrides. `--function-address N` selects an exact function-symbol
entry by its original virtual address, with the same range bounds and recursive
default. Matching aliases with the same sized section range agree; unsized aliases
are ignored when a sized alias supplies the range. Conflicting sized ranges fail
explicitly. Choose one of `--function`, `--function-address`, or `--section`.
For stripped or zero-sized symbols, select a known
range and entry explicitly, or recover their entire executable section.

## Recursive recovery and control-flow analysis

`lift --recursive` starts at the entry and follows both conditional paths, direct
jumps, and direct calls inside the selected range. Calls also retain their return
continuations. Returns, explicit traps and unconditional jumps stop fallthrough. Bytes skipped by
all discovered paths are left out of the IR. The result stays sorted and uses the
same IR format and backends as linear decoding.

Use `--no-follow-calls` with `--recursive` to inspect a caller without descending
into callees. `--max-instructions N` bounds every lift mode, including default
linear decoding, explicit `--linear`, `--recursive` and `--all-code`. The default
is 1,000,000 instructions and N must be positive. Raise it explicitly for larger
inputs. The limit applies across all sections in `--all-code` mode.
Targets outside the selected range remain external. Indirect calls/jumps are
preserved but their destinations are not guessed during decoding. Later value
analysis can resolve a constant destination already present in the IR.
Reachable unsupported or
truncated instructions, overlapping decodes, and branches into decoded instruction
interiors fail with an address; no partial output is written.

Try `examples/recovery.hex`, which jumps over four embedded data bytes, calls a
function, and multiplies 6 by 7. Its recovered IR is in `examples/recovery.ir`:

```sh
cargo run -- lift --hex "$(cat examples/recovery.hex)" --base 0x1000 --recursive -o recovery.ir
cargo run -- analyze recovery.ir
cargo run -- analyze recovery.ir --to dot -o recovery.dot
cargo run -- analyze recovery.ir --to json -o recovery.json
# Optional visualization, if Graphviz is installed:
dot -Tsvg recovery.dot -o recovery.svg
```

`analyze` independently reads any valid IR, including hand-written or linear IR:

- Blocks split at entry, direct destinations, control transfers, and address gaps.
- Edges distinguish taken branches, fallthrough, jumps, calls, returns, traps and REP repetition.
- Resolution distinguishes internal instruction boundaries, external addresses,
  instruction interiors, unknown indirect destinations, and dynamic returns.
- Reachability follows known edges from entry, conservatively assuming calls can
  return. Unreachable instructions in linear IR stay visible.
- Function entries are candidates from entry and internal direct/inferred call targets;
  they do not establish function boundaries or ownership.
- Loops are cyclic strongly connected components of the CFG excluding call edges.
  They include self-loops and may combine multiple nested source-level loops.

JSON uses schema `rosemary-analysis-1`. All addresses are `0x` hexadecimal strings
to preserve 64-bit precision. Blocks contain instruction addresses, sizes,
mnemonics and assembly; edges contain source block, instruction site, target,
kind and resolution. `inferred` marks destinations recovered from values;
`feasible` reports a conditional edge's inferred outcome, or `null` when unknown.
Unknown targets are `null`. DOT shows unreachable blocks
with dashed borders, call edges with dashed lines, and infeasible branches in gray.
The `value_analysis` field adds the `rosemary-values-1` report. These are static,
conservative results; indirect transfers and possible nonreturning calls can
limit reachability accuracy.

## Explaining and simplifying machine code

```sh
cargo run -- analyze examples/proven_branch.ir --to explain
cargo run -- simplify examples/proven_branch.ir -o /tmp/simplified.ir
cargo run -- emit /tmp/simplified.ir --to c -o /tmp/simplified.c
```

The example clears EAX, conditionally branches, and multiplies 6 by 7. Rosemary
explains that ZF=1 makes JNE always fall through and that RAX is 42 at the return.
The other return has no path from entry under the value model. `simplify`
replaces that JNE with a NOP while retaining its original IR address and length.
It also replaces always-taken flag branches and JRCXZ with jumps and constant
indirect calls/jumps with direct transfers. LOOP/LOOPE/LOOPNE remain intact even
when their branch outcome is proven, preserving their RCX decrement. It retains
other instructions, including unreachable instructions and their labels.

`examples/path_facts.ir` begins with unknown flags. The first JE stays
conditional, but each of its paths determines ZF, so later JNE instructions can
be simplified and the value model finds no feasible path to either UD2. Inspect
it with `rosemary analyze examples/path_facts.ir --to explain`.

The worklist engine reaches a fixed point over constants/unknown values for all
16 general registers, ZF/SF/CF/OF/PF and the direction flag. A join keeps a constant only when every
incoming state agrees. Loop revisits can invalidate early conclusions. The
chosen outcome of each branch refines its successor's flag constants. A taken
JRCXZ supplies RCX=0, and a LOOP exit supplies the decremented RCX=0; conditional
LOOP variants combine counter and ZF facts. Only independent constants are
retained: correlations such as SF differing from OF remain outside the domain.
The analysis tracks wrapping arithmetic, zero/sign extension, flag preservation,
effective addresses, conditional moves, shifts, and multiplication overflow.
It starts with unknown input registers and flags; no user input is executed.
XMM contents and FS/GS base values are not tracked as constants. REP uses a
self-edge per element, joining partial resumptions rather than assuming the
initial counter still holds when the same instruction is revisited.

Memory loads remain unknown even after a store to the same address; the engine
does not assume aliasing, contents, or a memory map. Calls discard return-state
register/flag knowledge, and callees start with unknown state. A reachable
unresolved indirect call/jump seeds unknown input at **every decoded instruction**
to avoid inferring facts from incomplete incoming edges. Architectural undefined
flags also become unknown. Returns assume normal call/return discipline; arbitrary
ROP transfers, exceptions, callbacks into arbitrary instruction addresses, and
host resumes at additional entry points are outside this model. Simplifications
apply to execution from the declared entry under those assumptions.

Value JSON distinguishes unreachable instructions from reachable unknown values.
It includes known registers and flags after each instruction, conditional branch
outcomes, resolved control targets and explanations. Address/register constants
use hexadecimal strings; absent constants and `null` outcomes mean unknown.

## Dogfooding: complete self-reversal

```sh
cargo build --release
target/release/rosemary lift target/release/rosemary --all-code -o /tmp/rosemary.ir
target/release/rosemary analyze /tmp/rosemary.ir --to summary
target/release/rosemary emit /tmp/rosemary.ir --to asm -o /tmp/rosemary.s

# Strict build/self-scan/full-code recovery, full ASM assembly,
# and C compilation of real core functions including CLI run/main.
python3 scripts/dogfood.py

# Optional independent instruction-boundary check:
python3 scripts/dogfood.py --verify-boundaries

cargo test --test dogfood
cargo test --release --test dogfood
```

The script freezes the subject as `target/dogfood/subject.elf`. Its
`coverage.json` records the exact FNV-1a fingerprint, execution entry, executable
sections, instruction count, byte coverage, and compiled core samples. It requires
every byte of every executable PROGBITS section to decode, every sized function
to lift linearly, and the full reconstructed assembly to assemble. Unsized startup
symbols remain explicit in the symbol report and are recovered through their
sections. No canary function is added.

`rosemary.ir` and `rosemary.s` contain the full recovered machine code;
`summary.txt` contains control-flow totals. `all-functions.json` covers the whole
symbol inventory; `report.json` focuses on matching names; `samples.json` indexes
IR/C/ASM, explanations, JSON and DOT for selected core functions. Samples prioritize
CLI `run`/`main`, the x86 decoder and value transfer. These C samples compile as
machine-state translations with guest callbacks, without recreating a native OS
loader, library ABI or the original source variables/types.

`--verify-boundaries` uses GNU objdump only as an independent test oracle after
Rosemary has decoded the code. Every instruction address, length and normalized mnemonic must agree.
Normal builds, runtime and the default dogfood decoder do not use an external
disassembler. Python, `cc` and the optional oracle are development tools only.

`scan` defaults to recursive recovery; `--recursive` selects that mode explicitly.
`--linear` measures every byte in each
symbol instead. It parses the ELF once and records per-function errors rather
than aborting a batch. `--filter TEXT` matches raw symbol names; the default
per-function instruction limit is 10,000. Reports include the chosen mode,
instruction limit, decoded bytes, block counts, proven branches, inferred targets,
unresolved transfers and grouped errors. Aliases count as separate symbols.
Zero-sized symbols require an explicit range or whole-section recovery.

Complete byte coverage is distinct from complete control-flow or ABI recovery:
indirect transfers remain explicit even when all possible instruction addresses
have been decoded. The fingerprint identifies an input for comparisons and is
not a cryptographic integrity check. Inventories and coverage can change with
compiler versions, build profiles and source changes.

## IR format

The IR is a machine-level representation with explicit operands, widths,
addresses, and instruction lengths. It can be edited or generated independently
of the decoder. For example:

```text
rosemary-ir 1
entry 1000
1000 5 mov r:32:0 i:32:2a
1005 1 ret
```

Instruction lines have the form `ADDRESS LENGTH OP [OPERAND ...]`. Addresses and
immediate/displacement values are hexadecimal without `0x`; lengths, widths,
register indexes, and scales are decimal. Blank lines and `#` comments are
accepted.

| Operand | Representation |
| --- | --- |
| Register | `r:WIDTH:INDEX` |
| High byte AH/CH/DH/BH | `h:8:INDEX` (0..3) |
| XMM register | `x:128:INDEX` (0..15) |
| Immediate | `i:WIDTH:HEX_VALUE` |
| Memory | `m:WIDTH:BASE:INDEX:SCALE:HEX_DISPLACEMENT` |
| FS/GS memory | Same memory operand followed by `:fs` or `:gs` |

Register indexes 0 through 15 correspond to `rax rcx rdx rbx rsp rbp rsi rdi
r8 r9 r10 r11 r12 r13 r14 r15`. A width of 32 selects the corresponding 32-bit
register; writes clear the upper 32 bits. Widths 8 and 16 select the low byte/word
and preserve the other register bits. Byte indexes 4..7 mean SPL/BPL/SIL/DIL;
high-byte operands use their separate `h:8:INDEX` encoding. Shift counts use `i:8:VALUE` or
`r:8:1` (CL). Sign/zero-extension moves have different source/destination widths.
Memory base/index use 64-bit registers;
`-` means absent. Scale is 1, 2, 4, or 8. Displacements use 64-bit two's-complement
values, and effective addresses wrap at 64 bits. RIP-relative addresses are
resolved into absolute displacement values during lifting. Memory width 128 is
reserved for supported vector operations. A segment suffix adds the guest FS/GS
base to the effective address; it never accesses the host's TLS implicitly.
`lock.OP` preserves an atomic prefix, and `rep.movs i:8:WIDTH` /
`rep.stos i:8:WIDTH` encode implicit-register string operations.

The parser validates operand shapes, register indexes, widths, instruction
lengths, sorted nonoverlapping address ranges, and entry boundaries. Branches
may leave the translated region.

Near returns use `ret` or `ret i:16:HEX_BYTES`. For example, `ret i:16:10`
pops the 64-bit return address and then releases 16 additional stack bytes.
The cleanup is an unsigned byte count from 0 to 65,535 and preserves all flags.

## C output

The C backend emits C11 code containing `rosemary_machine` and
`rosemary_run(machine, instruction_budget)`. This is executable machine-state
translation, not high-level recovery of original variables, types, or functions.
Control flow uses a switch over the emulated instruction pointer.

To run the result:

1. Zero-initialize a `rosemary_machine` and set `rip` to `ROSEMARY_ENTRY`.
2. Initialize registers, XMM state (`x[16]`, each with `lo`/`hi` words), stack,
   incoming flags (0 or 1), and `df`. Set `fs_base`/`gs_base` when segmented memory is used.
3. Provide `load` and `store` callbacks for little-endian emulated memory.
   Callbacks receive byte widths of 1, 2, 4 or 8. They must not modify machine state.
4. Call `rosemary_run` with a finite instruction budget. It returns 0 when control
   reaches an address outside the translated instructions, or 1 when the budget
   is exhausted. It returns 2 for an explicit guest trap/stop or division by zero /
   quotient overflow, leaving RIP at the faulting instruction. State remains
   available for inspection or resumption.

Calls and returns use the emulated stack. Place an address outside the translated
region on the stack to serve as the final return sentinel. `ret imm16` reads
that address at the original RSP, then advances RSP by 8 plus the unsigned cleanup
count, wrapping at 64 bits. Discarded stack bytes do not trigger memory callbacks.
For a memory destination, POP computes its effective address after advancing RSP;
value-analysis explanations use the same address as the generated C store.
Code does not execute
native machine instructions or directly dereference guest addresses. The host
callbacks are responsible for memory bounds, mapping, and faults. Full 128-bit
vector memory accesses use two little-endian 8-byte callback operations.
MOVLPS/MOVHPS/MOVLPD/MOVHPD use one 8-byte callback; loads preserve the untouched
64-bit XMM half and stores transfer only the selected half. Alignment faults
and atomic visibility are not modeled. LOCK/implicit atomic XCHG retain sequential
effects; multi-threaded guest execution requires a stronger host memory model.

The backend models ZF, SF, CF, OF, and PF for the supported arithmetic and logical
operations, preserving flags for moves, stack operations, and control transfers.
`inc`/`dec` preserve CF, `not` preserves all flags, and `imul` sets CF/OF on signed
overflow while preserving its architecturally undefined ZF/SF/PF. Signed products
use unsigned C arithmetic and magnitude checks, avoiding signed overflow in C.
Shifts mask counts to 5 or 6 bits and preserve flags for a zero count. For
architecturally undefined shift flags, generated C keeps OF and uses the last
shifted bit for CF; the value analyzer forgets undefined flags. Byte/word writes
preserve upper register bits; 32-bit writes zero-extend, including zero-count
shifts and conditional moves whose condition is false.
Wide multiplication/division use portable unsigned C helpers without requiring
a C compiler extension for 128-bit integers. Undefined results/flags are not used
as facts by the value analyzer.

Carry rotates (`rcl`/`rcr`) include CF as an extra bit. Counts are masked to 5 or
6 bits, then reduced modulo 9 or 17 for byte/word operands. A zero effective
count preserves all flags. For nonzero rotations, OF is defined only when the
masked count is 1; other OF results remain unknown in value analysis, while
generated C preserves the previous OF. CLC/STC/CMC change only CF. These rules
follow the [Intel instruction reference](https://cdrdv2-public.intel.com/671110/325383-sdm-vol-2abcd.pdf).

Counter branches preserve all flags. LOOP/LOOPE/LOOPNE decrement RCX with 64-bit
wrapping before testing it; LOOPE/LOOPNE also test the incoming ZF. JRCXZ tests
RCX without changing it. Value analysis combines counter and flag information,
including partial facts when either condition proves a branch falls through.

REP advances one element per budget unit and leaves RIP at the same instruction
while RCX remains nonzero. This bounds long copies/fills and supports resumption
in either direction. A zero counter performs no memory access. Direction changes
preserve the arithmetic flags.

AF, other architectural exceptions, hardware memory behavior, and ABI/library call
reconstruction are outside the current model. An external call stops at its
external target after pushing its return address; the host can handle it and
resume explicitly. A branch into an instruction's interior also leaves the
translated region.

## Assembly output

The assembly backend emits GNU-style directives with Intel syntax, instruction
labels, and comments showing original addresses. Branches to decoded instructions
use labels. Counter branches use a local trampoline so their rel8 encoding can
reach reconstructed targets even when instruction sizes grow. This is an assembly
listing/reconstruction, not byte-for-byte binary
recovery: encoding choices may differ, RIP-relative memory has been normalized
to absolute addresses, and external targets and data addresses retain their
original numeric values. Relocating or linking output that references original
addresses requires the caller to arrange the corresponding memory layout.

## Example: summation loop

`examples/sum.hex` contains a small loop that computes `5 + 4 + 3 + 2 + 1` in EAX.
Its IR is also available in `examples/sum.ir`.

```sh
cargo run -- emit examples/sum.ir --to c -o /tmp/sum.c
cp examples/sum_host.c /tmp/sum_host.c
cc -std=c11 -Wall -Wextra -Werror /tmp/sum_host.c -o /tmp/rosemary-sum
/tmp/rosemary-sum
```

The host example prints `rax=15`. A C compiler is used here to run generated
output, not by Rosemary itself.

## Development and verification

```sh
cargo fmt --check
cargo test
cargo clippy --all-targets -- -D warnings
```

Integration tests cover IR round trips and validation, register/address decoding,
malformed and truncated inputs, ELF bounds checks, the CLI pipeline, and compiled
C execution of loops, flags, stack operations, memory accesses, and calls/returns.
They also cover recursive recovery, overlap rejection, call policies, instruction
limits, CFG reachability, uncertain edges, and a 20,000-block cycle without host
recursion. On x86-64, arithmetic results and defined flags are compared against
native CPU instructions across boundary and deterministic random inputs, and a
compiler-built ELF is lifted recursively and executed as generated C. Tests also
assemble generated listings, compare all integer condition codes, verify
inferred facts at every executed instruction, compare original/simplified
registers/flags/memory one step at a time, and reverse every executable byte and
sized function of the actual tool binary in debug and release builds. Additional
native tests cover SSE/SSE2 register/memory semantics, all shuffle selectors,
wide multiply/divide and traps, bit counts (including all POPCNT flags), carry
arithmetic, rotates/double shifts, high bytes, FS TLS addressing, LOCK updates
and REP copies/fills in both directions.
Tests require `cc` on PATH; normal builds
and runtime do not. The test compiler is used only to validate generated output.

The implementation separates decoding (`src/decode.rs`), typed IR (`src/ir.rs`),
ELF/symbol input (`src/elf.rs`), control flow (`src/analysis.rs`), value inference
(`src/dataflow.rs`), batch scanning (`src/scan.rs`), output (`src/emit.rs`), and
CLI handling (`src/main.rs`). Extended integer, vector, and string semantics are
isolated in `src/integer.rs`, `src/vector.rs` and `src/strings.rs`. All runtime functionality remains native Rust
without third-party crates or external disassemblers.
