use crate::Result;
use std::{borrow::Cow, fmt::Write};
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Segment {
    Fs,
    Gs,
}
impl Segment {
    pub fn name(self) -> &'static str {
        match self {
            Self::Fs => "fs",
            Self::Gs => "gs",
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Operand {
    Reg(u8, u8), // index, bit width
    High(u8),    // AH, CH, DH, BH: bits 8..15 of register 0..3
    Vec(u8),     // 128-bit XMM register
    Imm(u64, u8),
    Mem {
        segment: Option<Segment>,
        width: u8,
        base: Option<u8>,
        index: Option<u8>,
        scale: u8,
        disp: u64,
    },
}
impl Operand {
    pub fn width(&self) -> u8 {
        match self {
            Self::Reg(_, w) | Self::Imm(_, w) => *w,
            Self::Vec(_) => 128,
            Self::High(_) => 8,
            Self::Mem { width, .. } => *width,
        }
    }
    fn encode(&self) -> String {
        match self {
            Self::Reg(r, w) => format!("r:{w}:{r}"),
            Self::Vec(r) => format!("x:128:{r}"),
            Self::High(r) => format!("h:8:{r}"),
            Self::Imm(v, w) => format!("i:{w}:{v:x}"),
            Self::Mem {
                segment,
                width,
                base,
                index,
                scale,
                disp,
            } => {
                format!(
                    "m:{width}:{}:{}:{scale}:{disp:x}",
                    base.map_or("-".into(), |v| v.to_string()),
                    index.map_or("-".into(), |v| v.to_string())
                ) + &segment.map_or(String::new(), |s| format!(":{}", s.name()))
            }
        }
    }
    fn parse(s: &str) -> Result<Self> {
        let p: Vec<_> = s.split(':').collect();
        let dec = |s: &str| s.parse::<u8>().map_err(|_| format!("invalid number: {s}"));
        let hex = |s: &str| u64::from_str_radix(s, 16).map_err(|_| format!("invalid hex: {s}"));
        let reg = |s: &str| if s == "-" { Ok(None) } else { dec(s).map(Some) };
        match p.as_slice() {
            ["r", w, r] => Ok(Self::Reg(dec(r)?, dec(w)?)),
            ["x", "128", r] => Ok(Self::Vec(dec(r)?)),
            ["h", "8", r] => Ok(Self::High(dec(r)?)),
            ["i", w, v] => Ok(Self::Imm(hex(v)?, dec(w)?)),
            ["m", w, b, i, s, d] => Ok(Self::Mem {
                segment: None,
                width: dec(w)?,
                base: reg(b)?,
                index: reg(i)?,
                scale: dec(s)?,
                disp: hex(d)?,
            }),
            ["m", w, b, i, s, d, segment] => Ok(Self::Mem {
                segment: Some(match *segment {
                    "fs" => Segment::Fs,
                    "gs" => Segment::Gs,
                    _ => return Err("unknown memory segment".into()),
                }),
                width: dec(w)?,
                base: reg(b)?,
                index: reg(i)?,
                scale: dec(s)?,
                disp: hex(d)?,
            }),
            _ => Err(format!("invalid operand: {s}")),
        }
    }
    fn valid(&self) -> bool {
        matches!(self.width(), 8 | 16 | 32 | 64 | 128)
            && match self {
                Self::Reg(r, w) => *r < 16 && *w <= 64,
                Self::Vec(r) => *r < 16,
                Self::High(r) => *r < 4,
                Self::Imm(v, w) => *w <= 64 && (*w == 64 || *v < (1u64 << w)),
                Self::Mem {
                    base, index, scale, ..
                } => {
                    base.is_none_or(|r| r < 16)
                        && index.is_none_or(|r| r < 16)
                        && matches!(scale, 1 | 2 | 4 | 8)
                }
            }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Instruction {
    pub address: u64,
    pub len: u8,
    pub op: String,
    pub operands: Vec<Operand>,
}
impl Instruction {
    /// Bytes removed from RSP by a validated near return, including its address.
    pub(crate) fn return_stack_bytes(&self) -> u64 {
        8 + match self.operands.as_slice() {
            [Operand::Imm(extra, 16)] => *extra,
            _ => 0,
        }
    }

    /// Architectural operation without the retained LOCK or NOTRACK prefix.
    pub fn mnemonic(&self) -> &str {
        self.op
            .strip_prefix("lock.")
            .or_else(|| self.op.strip_prefix("notrack."))
            .unwrap_or(&self.op)
    }

    pub(crate) fn without_prefix(&self) -> Cow<'_, Self> {
        let op = self.mnemonic();
        if op != self.op {
            let mut i = self.clone();
            i.op = op.into();
            Cow::Owned(i)
        } else {
            Cow::Borrowed(self)
        }
    }
    pub fn direct_target(&self) -> Option<u64> {
        if self.is_control_flow()
            && let [Operand::Imm(target, 64)] = self.operands.as_slice()
        {
            Some(*target)
        } else {
            None
        }
    }

    pub fn is_conditional(&self) -> bool {
        let op = self.mnemonic();
        CONDITIONS.contains(&op) || matches!(op, "loop" | "loope" | "loopne" | "jrcxz")
    }

    pub(crate) fn is_counter_loop(&self) -> bool {
        matches!(self.mnemonic(), "loop" | "loope" | "loopne")
    }

    pub fn is_control_flow(&self) -> bool {
        self.is_conditional()
            || matches!(
                self.mnemonic(),
                "jmp" | "call" | "ret" | "ud2" | "int3" | "hlt" | "rep.movs" | "rep.stos"
            )
    }

    pub fn falls_through(&self) -> bool {
        !matches!(self.mnemonic(), "jmp" | "ret" | "ud2" | "int3" | "hlt")
    }
}
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Program {
    pub entry: u64,
    pub instructions: Vec<Instruction>,
}
pub const CONDITIONS: [&str; 16] = [
    "jo", "jno", "jb", "jae", "je", "jne", "jbe", "ja", "js", "jns", "jp", "jnp", "jl", "jge",
    "jle", "jg",
];
pub fn condition_code(op: &str) -> Option<&'static str> {
    CONDITIONS.iter().copied().find(|j| {
        op == *j
            || op.strip_prefix("cmov") == j.strip_prefix('j')
            || op.strip_prefix("set") == j.strip_prefix('j')
    })
}
impl Program {
    pub fn validate(&self) -> Result<()> {
        if self.instructions.is_empty() {
            return Err("empty program".into());
        }
        let mut end = None;
        for i in &self.instructions {
            let fail = || format!("invalid IR instruction at 0x{:x}", i.address);
            if !(1..=15).contains(&i.len) || end.is_some_and(|e| i.address < e) {
                return Err(fail());
            }
            end = Some(i.address.checked_add(i.len.into()).ok_or_else(fail)?);
            if !i.operands.iter().all(Operand::valid) {
                return Err(fail());
            }
            if let Some(op) = i.op.strip_prefix("lock.")
                && (!matches!(
                    op,
                    "add"
                        | "adc"
                        | "sub"
                        | "sbb"
                        | "and"
                        | "or"
                        | "xor"
                        | "inc"
                        | "dec"
                        | "neg"
                        | "not"
                        | "bts"
                        | "btr"
                        | "btc"
                        | "xadd"
                        | "cmpxchg"
                        | "xchg"
                ) || !matches!(i.operands.first(), Some(Operand::Mem { .. })))
            {
                return Err(fail());
            }
            if let Some(op) = i.op.strip_prefix("notrack.")
                && (!matches!(op, "jmp" | "call")
                    || !matches!(
                        i.operands.as_slice(),
                        [Operand::Reg(_, 64)
                            | Operand::Mem {
                                width: 64,
                                segment: None,
                                ..
                            }]
                    ))
            {
                return Err(fail());
            }
            let unprefixed = i.without_prefix();
            let i = unprefixed.as_ref();
            let a = &i.operands;
            let writable = |o: &Operand| !matches!(o, Operand::Imm(..));
            let vector_valid = crate::vector::valid(i)
                .or_else(|| crate::integer::valid(i))
                .or_else(|| crate::strings::valid(i));
            let valid = if let Some(valid) = vector_valid {
                valid
            } else {
                match i.op.as_str() {
                    "nop" | "pause" | "endbr64" | "leave" | "ud2" | "int3" | "hlt" => a.is_empty(),
                    "ret" => a.is_empty() || matches!(a.as_slice(), [Operand::Imm(_, 16)]),
                    "inc" | "dec" | "neg" | "not" => a.len() == 1 && writable(&a[0]),
                    "push" => a.len() == 1 && a[0].width() == 64,
                    "pop" => a.len() == 1 && a[0].width() == 64 && writable(&a[0]),
                    "jmp" | "call" => a.len() == 1 && a[0].width() == 64,
                    _ if i.is_conditional() => matches!(a.as_slice(), [Operand::Imm(_, 64)]),
                    s if s.starts_with("cmov") && condition_code(s).is_some() => {
                        matches!(a.as_slice(), [Operand::Reg(_, w), src] if *w >= 16 && src.width() == *w && writable(src))
                    }
                    s if s.starts_with("set") && condition_code(s).is_some() => {
                        a.len() == 1 && a[0].width() == 8 && writable(&a[0])
                    }
                    "shl" | "shr" | "sar" => {
                        a.len() == 2
                            && writable(&a[0])
                            && matches!(a[1], Operand::Imm(_, 8) | Operand::Reg(1, 8))
                    }
                    "mov" | "add" | "sub" | "xor" | "and" | "or" | "cmp" | "test" => {
                        a.len() == 2
                            && writable(&a[0])
                            && a[0].width() == a[1].width()
                            && !(matches!(a[0], Operand::Mem { .. })
                                && matches!(a[1], Operand::Mem { .. }))
                    }
                    "lea" => {
                        matches!(
                            a.as_slice(),
                            [Operand::Reg(..), Operand::Mem { segment: None, .. }]
                        ) && a[0].width() == a[1].width()
                            && a[0].width() >= 16
                    }
                    "movzx" | "movsx" => {
                        matches!(a.as_slice(), [Operand::Reg(_, w), src] if writable(src) && matches!(src.width(), 8 | 16) && src.width() < *w)
                    }
                    "movsxd" => {
                        matches!(a.as_slice(), [Operand::Reg(_, 64), src] if writable(src) && src.width() == 32)
                    }
                    "imul" => {
                        matches!(a.as_slice(), [Operand::Reg(_, w), src] if *w >= 16 && *w == src.width() && writable(src))
                            || matches!(a.as_slice(), [Operand::Reg(_, w), src, Operand::Imm(_, iw)] if *w >= 16 && *w == src.width() && *w == *iw && writable(src))
                    }
                    _ => false,
                }
            };
            let valid = valid && (vector_valid.is_some() || a.iter().all(|o| o.width() <= 64));
            if !valid {
                return Err(fail());
            }
        }
        if !self.instructions.iter().any(|i| i.address == self.entry) {
            return Err("entry must be an instruction boundary".into());
        }
        Ok(())
    }
    pub fn to_text(&self) -> Result<String> {
        self.validate()?;
        let mut s = format!("rosemary-ir 1\nentry {:x}\n", self.entry);
        for i in &self.instructions {
            write!(s, "{:x} {} {}", i.address, i.len, i.op).unwrap();
            for o in &i.operands {
                write!(s, " {}", o.encode()).unwrap();
            }
            s.push('\n');
        }
        Ok(s)
    }
    pub fn from_text(text: &str) -> Result<Self> {
        let mut lines = text
            .lines()
            .map(|s| s.split('#').next().unwrap().trim())
            .filter(|s| !s.is_empty());
        if lines.next() != Some("rosemary-ir 1") {
            return Err("expected rosemary-ir 1 header".into());
        }
        let e = lines
            .next()
            .ok_or("missing entry")?
            .strip_prefix("entry ")
            .ok_or("missing entry")?;
        let entry = u64::from_str_radix(e, 16).map_err(|_| "invalid entry")?;
        let mut instructions = Vec::new();
        for line in lines {
            let p: Vec<_> = line.split_whitespace().collect();
            if p.len() < 3 {
                return Err(format!("invalid IR line: {line}"));
            }
            instructions.push(Instruction {
                address: u64::from_str_radix(p[0], 16).map_err(|_| "invalid address")?,
                len: p[1].parse().map_err(|_| "invalid length")?,
                op: p[2].into(),
                operands: p[3..]
                    .iter()
                    .map(|s| Operand::parse(s))
                    .collect::<Result<_>>()?,
            });
        }
        let p = Self {
            entry,
            instructions,
        };
        p.validate()?;
        Ok(p)
    }
}
