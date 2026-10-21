use crate::{
    Result,
    ir::{CONDITIONS, Operand, Program, condition_code},
};
use std::collections::BTreeSet;
use std::fmt::Write;
const R64: [&str; 16] = [
    "rax", "rcx", "rdx", "rbx", "rsp", "rbp", "rsi", "rdi", "r8", "r9", "r10", "r11", "r12", "r13",
    "r14", "r15",
];
const R32: [&str; 16] = [
    "eax", "ecx", "edx", "ebx", "esp", "ebp", "esi", "edi", "r8d", "r9d", "r10d", "r11d", "r12d",
    "r13d", "r14d", "r15d",
];
const R16: [&str; 16] = [
    "ax", "cx", "dx", "bx", "sp", "bp", "si", "di", "r8w", "r9w", "r10w", "r11w", "r12w", "r13w",
    "r14w", "r15w",
];
const R8: [&str; 16] = [
    "al", "cl", "dl", "bl", "spl", "bpl", "sil", "dil", "r8b", "r9b", "r10b", "r11b", "r12b",
    "r13b", "r14b", "r15b",
];
pub(crate) fn asm_operand(o: &Operand) -> String {
    match o {
        Operand::Reg(r, w) => match w {
            64 => R64[*r as usize],
            32 => R32[*r as usize],
            16 => R16[*r as usize],
            _ => R8[*r as usize],
        }
        .into(),
        Operand::Vec(r) => format!("xmm{r}"),
        Operand::High(r) => ["ah", "ch", "dh", "bh"][*r as usize].into(),
        Operand::Imm(v, _) => format!("0x{v:x}"),
        Operand::Mem {
            segment,
            width,
            base,
            index,
            scale,
            disp,
        } => {
            let mut parts = Vec::new();
            if let Some(b) = base {
                parts.push(R64[*b as usize].to_string());
            }
            if let Some(i) = index {
                parts.push(format!("{}*{scale}", R64[*i as usize]));
            }
            // Unsigned constants represent the same wrapping 64-bit displacement.
            if *disp != 0 || parts.is_empty() {
                parts.push(format!("0x{disp:x}"));
            }
            format!(
                "{} ptr {}[{}]",
                match width {
                    128 => "xmmword",
                    64 => "qword",
                    32 => "dword",
                    16 => "word",
                    _ => "byte",
                },
                segment.map_or(String::new(), |s| format!("{}:", s.name())),
                parts.join(" + ")
            )
        }
    }
}
pub fn assembly(p: &Program) -> Result<String> {
    p.validate()?;
    let addresses: BTreeSet<_> = p.instructions.iter().map(|i| i.address).collect();
    let mut s = String::from(".intel_syntax noprefix\n.text\n.global rosemary_entry\n");
    for i in &p.instructions {
        if i.address == p.entry {
            s.push_str("rosemary_entry:\n");
        }
        writeln!(
            s,
            ".L_{:x}: # original address 0x{:x}",
            i.address, i.address
        )
        .unwrap();
        if crate::strings::valid(i).is_some() {
            writeln!(s, "    {}", crate::strings::assembly(i)).unwrap();
            continue;
        }
        let operands = i
            .operands
            .iter()
            .map(|o| {
                if (i.op == "jmp" || i.op == "call" || CONDITIONS.contains(&i.op.as_str()))
                    && let Operand::Imm(v, _) = o
                    && addresses.contains(v)
                {
                    return format!(".L_{v:x}");
                }
                // GAS requires signed immediates when encoding sign-extended imm32.
                if let Operand::Imm(v, 64) = o
                    && (*v as i64) < 0
                {
                    return format!("{}", *v as i64);
                }
                asm_operand(o)
            })
            .collect::<Vec<_>>()
            .join(", ");
        writeln!(s, "    {} {}", i.op.replace("lock.", "lock "), operands).unwrap();
    }
    Ok(s)
}
pub(crate) fn address(o: &Operand) -> String {
    if let Operand::Mem {
        segment,
        base,
        index,
        scale,
        disp,
        ..
    } = o
    {
        let mut s = format!("UINT64_C(0x{disp:x})");
        if let Some(segment) = segment {
            write!(s, " + m->{}_base", segment.name()).unwrap();
        }
        if let Some(b) = base {
            write!(s, " + m->r[{b}]").unwrap();
        }
        if let Some(i) = index {
            write!(s, " + m->r[{i}] * {scale}").unwrap();
        }
        format!("({s})")
    } else {
        unreachable!()
    }
}
pub(crate) fn read(o: &Operand) -> String {
    match o {
        Operand::Reg(r, 64) => format!("m->r[{r}]"),
        Operand::Reg(r, w) => format!("(m->r[{r}] & {})", mask(*w)),
        Operand::Imm(v, _) => format!("UINT64_C(0x{v:x})"),
        Operand::Vec(_) => unreachable!("vector operands use the vector backend"),
        Operand::High(r) => format!("((m->r[{r}]>>8)&UINT64_C(0xff))"),
        Operand::Mem { width, .. } => format!(
            "(m->load(m->ctx, {}, {}) & {})",
            address(o),
            width / 8,
            mask(*width)
        ),
    }
}
fn mask(w: u8) -> &'static str {
    match w {
        8 => "UINT64_C(0xff)",
        16 => "UINT64_C(0xffff)",
        32 => "UINT64_C(0xffffffff)",
        _ => "UINT64_MAX",
    }
}
pub(crate) fn put(o: &Operand, value: &str) -> String {
    match o {
        Operand::High(r) => {
            format!("m->r[{r}]=(m->r[{r}]&~UINT64_C(0xff00))|((({value})&255)<<8);")
        }
        Operand::Reg(r, w) if *w < 32 => format!(
            "m->r[{r}] = (m->r[{r}] & ~{}) | (({value}) & {});",
            mask(*w),
            mask(*w)
        ),
        Operand::Reg(r, w) => format!("m->r[{r}] = ({value}) & {};", mask(*w)),
        Operand::Mem { width, .. } => format!(
            "m->store(m->ctx, ea, {}, ({value}) & {});",
            width / 8,
            mask(*width)
        ),
        _ => unreachable!(),
    }
}
fn cond(op: &str) -> &'static str {
    match op {
        "jo" => "m->of",
        "jno" => "!m->of",
        "jb" => "m->cf",
        "jae" => "!m->cf",
        "je" => "m->zf",
        "jne" => "!m->zf",
        "jbe" => "m->cf || m->zf",
        "ja" => "!m->cf && !m->zf",
        "js" => "m->sf",
        "jns" => "!m->sf",
        "jp" => "m->pf",
        "jnp" => "!m->pf",
        "jl" => "m->sf != m->of",
        "jge" => "m->sf == m->of",
        "jle" => "m->zf || (m->sf != m->of)",
        "jg" => "!m->zf && (m->sf == m->of)",
        _ => unreachable!(),
    }
}
pub fn c(p: &Program) -> Result<String> {
    p.validate()?;
    let mut s = String::from(
        r#"/* Generated by rosemary. x86-64 state; r[] order:
   rax rcx rdx rbx rsp rbp rsi rdi r8 r9 r10 r11 r12 r13 r14 r15.
   Memory callbacks read/write little-endian values, widths 1, 2, 4 or 8 bytes.
   Supply valid callbacks and memory; memory faults are the host's responsibility.
   Set rip to ROSEMARY_ENTRY, initialize registers/flags, then run with a budget.
   Return: 0 = control left code, 1 = budget exhausted, 2 = explicit guest trap.
   REP advances one element per budget unit. LOCK uses the sequential memory
   callback model; host/guest concurrency and alignment faults are not modeled.
   RET pops an address from the emulated stack; use an outside sentinel to stop.
   Flags AF and architectural exceptions are not modeled. */
#include <stdint.h>
#include <stddef.h>
typedef struct { uint64_t lo, hi; } rosemary_vec;
typedef struct {
    uint64_t r[16], rip;
    rosemary_vec x[16];
    uint64_t fs_base, gs_base;
    unsigned zf, sf, cf, of, pf, df;
    void *ctx;
    uint64_t (*load)(void *, uint64_t, unsigned);
    void (*store)(void *, uint64_t, unsigned, uint64_t);
} rosemary_machine;
static inline void rosemary_flags(rosemary_machine *m, uint64_t a, uint64_t b,
                           uint64_t v, unsigned w, unsigned kind) {
    uint64_t sign = UINT64_C(1) << (w - 1);
    m->zf = v == 0; m->sf = (v & sign) != 0;
    unsigned x = (unsigned)(v & 255); x ^= x >> 4; x ^= x >> 2; x ^= x >> 1;
    m->pf = !(x & 1);
    m->cf = kind == 1 ? v < a : kind == 2 ? a < b : 0;
    m->of = kind == 1 ? ((~(a ^ b) & (a ^ v) & sign) != 0)
          : kind == 2 ? (((a ^ b) & (a ^ v) & sign) != 0) : 0;
}
"#,
    );
    s.push_str(crate::vector::HELPERS);
    s.push_str(crate::integer::HELPERS);
    writeln!(s, "#define ROSEMARY_ENTRY UINT64_C(0x{:x})", p.entry).unwrap();
    s.push_str("int rosemary_run(rosemary_machine *m, size_t budget) {\n    while (budget--) {\n        switch (m->rip) {\n");
    for original in &p.instructions {
        let unprefixed = original.without_lock();
        let i = unprefixed.as_ref();
        writeln!(
            s,
            "        case UINT64_C(0x{:x}): {{ /* {} */",
            i.address, i.op
        )
        .unwrap();
        let next = i.address + i.len as u64;
        let a = &i.operands;
        let line = |s: &mut String, t: String| {
            writeln!(s, "            {t}").unwrap();
        };
        let ea = |s: &mut String, o: &Operand| {
            if matches!(o, Operand::Mem { .. }) {
                line(s, format!("uint64_t ea = {};", address(o)));
            }
        };
        match i.op.as_str() {
            _ if crate::strings::valid(i).is_some() => line(&mut s, crate::strings::emit(i)),
            _ if crate::integer::valid(i).is_some() => line(&mut s, crate::integer::emit(i)),
            op if crate::vector::recognized(op) => line(&mut s, crate::vector::emit(i)),
            "nop" | "pause" | "endbr64" => {}
            "ud2" | "int3" | "hlt" => line(&mut s, "return 2; /* explicit guest trap */".into()),
            "mov" | "lea" | "movsxd" | "movsx" | "movzx" => {
                ea(&mut s, &a[0]);
                let v = if i.op == "lea" {
                    address(&a[1])
                } else if i.op == "movsxd" || i.op == "movsx" {
                    // Unsigned arithmetic gives portable two's-complement sign extension.
                    let sign = 1u64 << (a[1].width() - 1);
                    format!(
                        "(({} ^ UINT64_C(0x{sign:x})) - UINT64_C(0x{sign:x}))",
                        read(&a[1])
                    )
                } else {
                    read(&a[1])
                };
                line(&mut s, put(&a[0], &v));
            }
            "shl" | "shr" | "sar" => {
                ea(&mut s, &a[0]);
                let w = a[0].width();
                line(&mut s, format!("uint64_t a = {}, v = a;", read(&a[0])));
                line(
                    &mut s,
                    format!(
                        "unsigned count = (unsigned){} & {};",
                        read(&a[1]),
                        if w == 64 { 63 } else { 31 }
                    ),
                );
                line(&mut s, "if (count) {".into());
                line(&mut s, "unsigned carry = 0, overflow = m->of;".into());
                let sign = format!("UINT64_C(0x{:x})", 1u64 << (w - 1));
                let step = match i.op.as_str() {
                    "shl" => format!("carry = (v & {sign}) != 0; v = (v << 1) & {};", mask(w)),
                    "shr" => "carry = (unsigned)(v & 1); v >>= 1;".into(),
                    _ => format!("carry = (unsigned)(v & 1); v = (v >> 1) | (v & {sign});"),
                };
                line(
                    &mut s,
                    format!("for (unsigned j = 0; j < count; ++j) {{ {step} }}"),
                );
                line(
                    &mut s,
                    format!("rosemary_flags(m, 0, 0, v, {w}, 0); m->cf = carry;"),
                );
                let of = match i.op.as_str() {
                    "shl" => format!("((v & {sign}) != 0) ^ carry"),
                    "shr" => format!("(a & {sign}) != 0"),
                    _ => "0".into(),
                };
                line(&mut s, format!("m->of = count == 1 ? ({of}) : overflow;"));
                line(&mut s, "}".into());
                line(&mut s, put(&a[0], "v"));
            }
            op if op.starts_with("cmov") => {
                line(&mut s, format!("uint64_t source = {};", read(&a[1])));
                line(
                    &mut s,
                    format!(
                        "uint64_t v = ({}) ? source : {};",
                        cond(condition_code(op).unwrap()),
                        read(&a[0])
                    ),
                );
                line(&mut s, put(&a[0], "v"));
            }
            op if op.starts_with("set") => {
                ea(&mut s, &a[0]);
                line(
                    &mut s,
                    put(
                        &a[0],
                        &format!("({}) ? 1 : 0", cond(condition_code(op).unwrap())),
                    ),
                );
            }
            "inc" | "dec" | "neg" | "not" => {
                ea(&mut s, &a[0]);
                line(&mut s, format!("uint64_t a = {};", read(&a[0])));
                let expression = match i.op.as_str() {
                    "inc" => "a + 1",
                    "dec" => "a - 1",
                    "neg" => "UINT64_C(0) - a",
                    _ => "~a",
                };
                line(
                    &mut s,
                    format!("uint64_t v = ({expression}) & {};", mask(a[0].width())),
                );
                line(&mut s, put(&a[0], "v"));
                if i.op == "neg" {
                    line(
                        &mut s,
                        format!("rosemary_flags(m, 0, a, v, {}, 2);", a[0].width()),
                    );
                } else if i.op != "not" {
                    line(&mut s, "unsigned carry = m->cf;".into());
                    line(
                        &mut s,
                        format!(
                            "rosemary_flags(m, a, 1, v, {}, {});",
                            a[0].width(),
                            if i.op == "inc" { 1 } else { 2 }
                        ),
                    );
                    line(&mut s, "m->cf = carry;".into());
                }
            }
            "imul" => {
                let (lhs, rhs) = if a.len() == 3 {
                    (&a[1], &a[2])
                } else {
                    (&a[0], &a[1])
                };
                let w = a[0].width();
                line(
                    &mut s,
                    format!("uint64_t a = {}, b = {};", read(lhs), read(rhs)),
                );
                line(&mut s, format!("uint64_t sign = UINT64_C(1) << {};", w - 1));
                line(
                    &mut s,
                    format!("uint64_t ua = (a & sign) ? ((~a + 1) & {}) : a;", mask(w)),
                );
                line(
                    &mut s,
                    format!("uint64_t ub = (b & sign) ? ((~b + 1) & {}) : b;", mask(w)),
                );
                line(
                    &mut s,
                    "uint64_t limit = ((a ^ b) & sign) ? sign : sign - 1;".into(),
                );
                line(&mut s, "m->cf = m->of = ub != 0 && ua > limit / ub;".into());
                line(&mut s, put(&a[0], "a * b"));
            }
            "add" | "sub" | "xor" | "and" | "or" | "cmp" | "test" => {
                let writes = !matches!(i.op.as_str(), "cmp" | "test");
                if writes {
                    ea(&mut s, &a[0]);
                }
                line(
                    &mut s,
                    format!("uint64_t a = {}, b = {};", read(&a[0]), read(&a[1])),
                );
                let (symbol, kind) = match i.op.as_str() {
                    "add" => ("+", 1),
                    "sub" | "cmp" => ("-", 2),
                    "xor" => ("^", 0),
                    "or" => ("|", 0),
                    _ => ("&", 0),
                };
                line(
                    &mut s,
                    format!("uint64_t v = (a {symbol} b) & {};", mask(a[0].width())),
                );
                if writes {
                    line(&mut s, put(&a[0], "v"));
                }
                line(
                    &mut s,
                    format!("rosemary_flags(m, a, b, v, {}, {kind});", a[0].width()),
                );
            }
            "push" => {
                line(&mut s, format!("uint64_t v = {};", read(&a[0])));
                line(
                    &mut s,
                    "m->r[4] -= 8; m->store(m->ctx, m->r[4], 8, v);".into(),
                );
            }
            "pop" => {
                line(
                    &mut s,
                    "uint64_t v = m->load(m->ctx, m->r[4], 8); m->r[4] += 8;".into(),
                );
                ea(&mut s, &a[0]);
                line(&mut s, put(&a[0], "v"));
            }
            "leave" => line(
                &mut s,
                "m->r[4] = m->r[5]; m->r[5] = m->load(m->ctx, m->r[4], 8); m->r[4] += 8;".into(),
            ),
            "ret" => {
                line(
                    &mut s,
                    "m->rip = m->load(m->ctx, m->r[4], 8); m->r[4] += 8; continue;".into(),
                );
            }
            "jmp" | "call" => {
                line(&mut s, format!("uint64_t target = {};", read(&a[0])));
                if i.op == "call" {
                    line(
                        &mut s,
                        format!(
                            "m->r[4] -= 8; m->store(m->ctx, m->r[4], 8, UINT64_C(0x{next:x}));"
                        ),
                    );
                }
                line(&mut s, "m->rip = target; continue;".into());
            }
            op => {
                line(
                    &mut s,
                    format!(
                        "m->rip = ({}) ? {} : UINT64_C(0x{next:x}); continue;",
                        cond(op),
                        read(&a[0])
                    ),
                );
            }
        }
        if !matches!(
            i.op.as_str(),
            "ret" | "jmp" | "call" | "ud2" | "int3" | "hlt"
        ) && !CONDITIONS.contains(&i.op.as_str())
        {
            line(&mut s, format!("m->rip = UINT64_C(0x{next:x}); break;"));
        }
        s.push_str("        }\n");
    }
    s.push_str("        default: return 0;\n        }\n    }\n    return 1;\n}\n");
    Ok(s)
}
