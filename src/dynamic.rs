//! Bounded, concrete IR execution. Guest memory never addresses host memory.
use crate::{
    Result,
    analysis::{listing, quoted},
    ir::{Instruction, Operand, Program, Segment, condition_code},
};
use std::{collections::BTreeMap, fmt::Write};

pub const REGISTERS: [&str; 16] = [
    "rax", "rcx", "rdx", "rbx", "rsp", "rbp", "rsi", "rdi", "r8", "r9", "r10", "r11", "r12", "r13",
    "r14", "r15",
];

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Flags {
    pub zf: bool,
    pub sf: bool,
    pub cf: bool,
    pub of: bool,
    pub pf: bool,
    pub df: bool,
}

impl Flags {
    fn condition(self, op: &str) -> bool {
        match op {
            "jo" => self.of,
            "jno" => !self.of,
            "jb" => self.cf,
            "jae" => !self.cf,
            "je" => self.zf,
            "jne" => !self.zf,
            "jbe" => self.cf || self.zf,
            "ja" => !self.cf && !self.zf,
            "js" => self.sf,
            "jns" => !self.sf,
            "jp" => self.pf,
            "jnp" => !self.pf,
            "jl" => self.sf != self.of,
            "jge" => self.sf == self.of,
            "jle" => self.zf || self.sf != self.of,
            "jg" => !self.zf && self.sf == self.of,
            _ => unreachable!("validated condition"),
        }
    }

    fn result(&mut self, value: u64, width: u8) {
        self.zf = value == 0;
        self.sf = value & (1 << (width - 1)) != 0;
        self.pf = (value as u8).count_ones().is_multiple_of(2);
    }

    fn arithmetic(&mut self, a: u64, b: u64, carry: bool, width: u8, sub: bool) -> u64 {
        let v = if sub {
            a.wrapping_sub(b).wrapping_sub(u64::from(carry))
        } else {
            a.wrapping_add(b).wrapping_add(u64::from(carry))
        } & mask(width);
        self.result(v, width);
        self.cf = if sub {
            (a as u128) < b as u128 + u128::from(carry)
        } else {
            a as u128 + b as u128 + u128::from(carry) > mask(width) as u128
        };
        self.of = (if sub { a ^ b } else { !(a ^ b) } & (a ^ v)) & (1 << (width - 1)) != 0;
        v
    }

    fn json(self) -> String {
        format!(
            "{{\"zf\":{},\"sf\":{},\"cf\":{},\"of\":{},\"pf\":{},\"df\":{}}}",
            self.zf, self.sf, self.cf, self.of, self.pf, self.df
        )
    }
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct State {
    pub registers: [u64; 16],
    pub rip: u64,
    pub flags: Flags,
    pub fs_base: u64,
    pub gs_base: u64,
}

impl State {
    fn json(self) -> String {
        let registers = REGISTERS
            .iter()
            .zip(self.registers)
            .map(|(r, v)| format!("\"{r}\":\"0x{v:x}\""))
            .collect::<Vec<_>>()
            .join(",");
        format!(
            "{{\"rip\":\"0x{:x}\",\"registers\":{{{registers}}},\"flags\":{},\"fs_base\":\"0x{:x}\",\"gs_base\":\"0x{:x}\"}}",
            self.rip,
            self.flags.json(),
            self.fs_base,
            self.gs_base
        )
    }
}

#[derive(Clone, Debug)]
struct Region {
    bytes: Vec<u8>,
    writable: bool,
}

/// Explicit, nonoverlapping, readable guest mappings. Code is immutable IR,
/// separate from this data memory; self-modifying code is not modeled.
#[derive(Clone, Debug, Default)]
pub struct Memory {
    regions: BTreeMap<u64, Region>,
}

impl Memory {
    pub fn map(&mut self, address: u64, bytes: Vec<u8>, writable: bool) -> Result<()> {
        if bytes.is_empty() {
            return Err("memory mapping must not be empty".into());
        }
        let end = address
            .checked_add(bytes.len() as u64)
            .ok_or("memory mapping overflow")?;
        if self
            .regions
            .range(..end)
            .next_back()
            .is_some_and(|(start, r)| *start + r.bytes.len() as u64 > address)
        {
            return Err(format!("overlapping memory mapping at 0x{address:x}"));
        }
        self.regions.insert(address, Region { bytes, writable });
        Ok(())
    }

    fn check(&self, address: u64, bytes: u8, write: bool) -> Result<()> {
        if !matches!(bytes, 1 | 2 | 4 | 8) {
            return Err("memory access width must be 1, 2, 4 or 8 bytes".into());
        }
        for offset in 0..bytes {
            let a = address
                .checked_add(offset.into())
                .ok_or("memory access overflow")?;
            let (_, r) = self
                .regions
                .range(..=a)
                .next_back()
                .filter(|(start, r)| a - **start < r.bytes.len() as u64)
                .ok_or_else(|| {
                    format!(
                        "unmapped {} at 0x{a:x}",
                        if write { "write" } else { "read" }
                    )
                })?;
            if write && !r.writable {
                return Err(format!("read-only memory at 0x{a:x}"));
            }
        }
        Ok(())
    }

    pub fn load(&self, address: u64, bytes: u8) -> Result<u64> {
        self.check(address, bytes, false)?;
        let mut value = 0;
        for offset in 0..bytes {
            let a = address + u64::from(offset);
            let (start, r) = self.regions.range(..=a).next_back().unwrap();
            value |= u64::from(r.bytes[(a - start) as usize]) << (offset * 8);
        }
        Ok(value)
    }

    pub fn store(&mut self, address: u64, bytes: u8, value: u64) -> Result<()> {
        self.check(address, bytes, true)?;
        for offset in 0..bytes {
            let a = address + u64::from(offset);
            let (start, r) = self.regions.range_mut(..=a).next_back().unwrap();
            r.bytes[(a - start) as usize] = (value >> (offset * 8)) as u8;
        }
        Ok(())
    }
}

#[derive(Clone, Debug, Default)]
pub struct Machine {
    pub state: State,
    pub memory: Memory,
}

impl Machine {
    /// CLI default: zero state, 64 KiB stack, and an external return sentinel.
    pub fn with_stack(entry: u64) -> Self {
        let mut m = Self::default();
        m.state.rip = entry;
        m.memory.map(0x7fff_0000, vec![0; 65536], true).unwrap();
        m.state.registers[4] = 0x7fff_fff8;
        m.memory.store(m.state.registers[4], 8, u64::MAX).unwrap();
        m
    }

    fn address(&self, o: &Operand) -> u64 {
        let Operand::Mem {
            segment,
            base,
            index,
            scale,
            disp,
            ..
        } = o
        else {
            unreachable!("validated memory operand")
        };
        let segment = match segment {
            Some(Segment::Fs) => self.state.fs_base,
            Some(Segment::Gs) => self.state.gs_base,
            None => 0,
        };
        disp.wrapping_add(segment)
            .wrapping_add(base.map_or(0, |r| self.state.registers[r as usize]))
            .wrapping_add(index.map_or(0, |r| {
                self.state.registers[r as usize].wrapping_mul((*scale).into())
            }))
    }

    fn location(&self, o: &Operand) -> Location {
        match o {
            Operand::Reg(r, w) => Location::Register(*r, *w),
            Operand::High(r) => Location::High(*r),
            Operand::Imm(v, _) => Location::Immediate(*v),
            Operand::Mem { width, .. } => Location::Memory(self.address(o), *width / 8),
            Operand::Vec(_) => unreachable!("vector instructions rejected before execution"),
        }
    }

    fn read(&self, l: Location, accesses: &mut Vec<Access>) -> std::result::Result<u64, Stop> {
        Ok(match l {
            Location::Register(r, w) => self.state.registers[r as usize] & mask(w),
            Location::High(r) => (self.state.registers[r as usize] >> 8) & 255,
            Location::Immediate(v) => v,
            Location::Memory(address, bytes) => {
                let value = self
                    .memory
                    .load(address, bytes)
                    .map_err(Stop::MemoryFault)?;
                accesses.push(Access {
                    address,
                    bytes,
                    value,
                    write: false,
                });
                value
            }
        })
    }

    fn write(
        &mut self,
        l: Location,
        value: u64,
        accesses: &mut Vec<Access>,
    ) -> std::result::Result<(), Stop> {
        match l {
            Location::Register(r, w) => {
                let old = self.state.registers[r as usize];
                self.state.registers[r as usize] =
                    (if w < 32 { old & !mask(w) } else { 0 }) | (value & mask(w));
            }
            Location::High(r) => {
                let old = self.state.registers[r as usize];
                self.state.registers[r as usize] = (old & !0xff00) | ((value & 255) << 8);
            }
            Location::Memory(address, bytes) => {
                self.memory
                    .check(address, bytes, true)
                    .map_err(Stop::MemoryFault)?;
                accesses.push(Access {
                    address,
                    bytes,
                    value: value & mask(bytes * 8),
                    write: true,
                });
            }
            Location::Immediate(_) => unreachable!("validated destination"),
        }
        Ok(())
    }

    fn step(
        &mut self,
        i: &Instruction,
        accesses: &mut Vec<Access>,
    ) -> std::result::Result<Option<bool>, Stop> {
        let op = i.mnemonic();
        let a = &i.operands;
        if a.iter().any(|o| o.width() == 128) || crate::floating::valid(i).is_some() {
            return Err(Stop::Unsupported(i.op.clone()));
        }
        let next = i.address + u64::from(i.len);
        let mut target = next;
        let mut taken = None;
        match op {
            "nop" | "pause" | "endbr64" => {}
            "ud2" | "int3" | "hlt" => return Err(Stop::Trap(op.into())),
            "cld" | "std" => self.state.flags.df = op == "std",
            "clc" => self.state.flags.cf = false,
            "stc" => self.state.flags.cf = true,
            "cmc" => self.state.flags.cf = !self.state.flags.cf,
            "mov" | "movzx" | "movsx" | "movsxd" | "lea" => {
                let dst = self.location(&a[0]);
                let mut value = if op == "lea" {
                    self.address(&a[1])
                } else {
                    self.read(self.location(&a[1]), accesses)?
                };
                if matches!(op, "movsx" | "movsxd") {
                    value = signed(value, a[1].width()) as u64;
                }
                self.write(dst, value, accesses)?;
            }
            "add" | "adc" | "sub" | "sbb" | "cmp" | "xor" | "and" | "or" | "test" => {
                let dst = self.location(&a[0]);
                let left = self.read(dst, accesses)?;
                let right = self.read(self.location(&a[1]), accesses)?;
                let w = a[0].width();
                let value = if matches!(op, "add" | "adc" | "sub" | "sbb" | "cmp") {
                    let carry = matches!(op, "adc" | "sbb") && self.state.flags.cf;
                    self.state.flags.arithmetic(
                        left,
                        right,
                        carry,
                        w,
                        matches!(op, "sub" | "sbb" | "cmp"),
                    )
                } else {
                    let v = match op {
                        "xor" => left ^ right,
                        "or" => left | right,
                        _ => left & right,
                    };
                    self.state.flags.result(v, w);
                    self.state.flags.cf = false;
                    self.state.flags.of = false;
                    v
                };
                if !matches!(op, "cmp" | "test") {
                    self.write(dst, value, accesses)?;
                }
            }
            "inc" | "dec" | "neg" | "not" => {
                let dst = self.location(&a[0]);
                let left = self.read(dst, accesses)?;
                let carry = self.state.flags.cf;
                let w = a[0].width();
                let value = match op {
                    "not" => !left,
                    "neg" => self.state.flags.arithmetic(0, left, false, w, true),
                    _ => self.state.flags.arithmetic(left, 1, false, w, op == "dec"),
                };
                if matches!(op, "inc" | "dec") {
                    self.state.flags.cf = carry;
                }
                self.write(dst, value, accesses)?;
            }
            "shl" | "shr" | "sar" | "rol" | "ror" | "rcl" | "rcr" | "shld" | "shrd" => {
                self.shift(i, accesses)?;
            }
            "xchg" | "xadd" | "cmpxchg" => {
                let dst = self.location(&a[0]);
                let src = self.location(&a[1]);
                let left = self.read(dst, accesses)?;
                let right = self.read(src, accesses)?;
                let w = a[0].width();
                if op == "cmpxchg" {
                    let acc = Location::Register(0, w);
                    let value = self.read(acc, accesses)?;
                    self.state.flags.arithmetic(value, left, false, w, true);
                    if self.state.flags.zf {
                        self.write(dst, right, accesses)?;
                    } else {
                        self.write(acc, left, accesses)?;
                        if matches!(dst, Location::Memory(..)) {
                            self.write(dst, left, accesses)?;
                        }
                    }
                } else if op == "xadd" {
                    let value = self.state.flags.arithmetic(left, right, false, w, false);
                    self.write(src, left, accesses)?;
                    self.write(dst, value, accesses)?;
                } else {
                    self.write(dst, right, accesses)?;
                    self.write(src, left, accesses)?;
                }
            }
            "imul" if a.len() > 1 => {
                let lhs = if a.len() == 3 { &a[1] } else { &a[0] };
                let rhs = if a.len() == 3 { &a[2] } else { &a[1] };
                let left = self.read(self.location(lhs), accesses)?;
                let right = self.read(self.location(rhs), accesses)?;
                let w = a[0].width();
                let product = signed(left, w) * signed(right, w);
                let overflow = signed(product as u64 & mask(w), w) != product;
                self.state.flags.cf = overflow;
                self.state.flags.of = overflow;
                self.write(self.location(&a[0]), product as u64, accesses)?;
            }
            "mul" | "imul" | "div" | "idiv" => self.multiply_divide(i, accesses)?,
            "cwd" | "cdq" | "cqo" => {
                let w = match op {
                    "cwd" => 16,
                    "cdq" => 32,
                    _ => 64,
                };
                let v = if self.state.registers[0] & (1 << (w - 1)) != 0 {
                    u64::MAX
                } else {
                    0
                };
                self.write(Location::Register(2, w), v, accesses)?;
            }
            "bt" | "bts" | "btr" | "btc" => {
                let w = a[0].width();
                let index = self.read(self.location(&a[1]), accesses)?;
                let mut dst = self.location(&a[0]);
                if let Location::Memory(address, bytes) = dst
                    && matches!(a[1], Operand::Reg(..))
                {
                    let offset = (signed(index, w) >> w.trailing_zeros()) * i128::from(bytes);
                    dst = Location::Memory(address.wrapping_add(offset as u64), bytes);
                }
                let left = self.read(dst, accesses)?;
                let bit = 1u64 << (index & u64::from(w - 1));
                self.state.flags.cf = left & bit != 0;
                let value = match op {
                    "bts" => left | bit,
                    "btr" => left & !bit,
                    _ => left ^ bit,
                };
                if op != "bt" {
                    self.write(dst, value, accesses)?;
                }
            }
            "bswap" => {
                let dst = self.location(&a[0]);
                let value = self.read(dst, accesses)?.swap_bytes() >> (64 - a[0].width());
                self.write(dst, value, accesses)?;
            }
            "bsf" | "bsr" | "tzcnt" | "lzcnt" | "popcnt" => {
                let w = a[0].width();
                let value = self.read(self.location(&a[1]), accesses)?;
                let count = match op {
                    "popcnt" => value.count_ones(),
                    "bsf" | "tzcnt" => value.trailing_zeros().min(w.into()),
                    _ => value.leading_zeros() - (64 - u32::from(w)),
                };
                if op == "popcnt" {
                    self.state.flags.zf = value == 0;
                    self.state.flags.cf = false;
                    self.state.flags.of = false;
                    self.state.flags.sf = false;
                    self.state.flags.pf = false;
                } else if matches!(op, "bsf" | "bsr") {
                    self.state.flags.zf = value == 0;
                } else {
                    self.state.flags.cf = value == 0;
                    self.state.flags.zf = count == 0;
                }
                if value != 0 || !matches!(op, "bsf" | "bsr") {
                    let result = if op == "bsr" {
                        u32::from(w) - 1 - count
                    } else {
                        count
                    };
                    self.write(self.location(&a[0]), result.into(), accesses)?;
                }
            }
            "push" | "call" => {
                let value = self.read(self.location(&a[0]), accesses)?;
                self.state.registers[4] = self.state.registers[4].wrapping_sub(8);
                self.write(
                    Location::Memory(self.state.registers[4], 8),
                    if op == "call" { next } else { value },
                    accesses,
                )?;
                if op == "call" {
                    target = value;
                }
            }
            "pop" | "ret" | "leave" => {
                let stack = if op == "leave" {
                    self.state.registers[5]
                } else {
                    self.state.registers[4]
                };
                let value = self.read(Location::Memory(stack, 8), accesses)?;
                self.state.registers[4] = stack.wrapping_add(if op == "ret" {
                    i.return_stack_bytes()
                } else {
                    8
                });
                match op {
                    "pop" => self.write(self.location(&a[0]), value, accesses)?,
                    "leave" => self.state.registers[5] = value,
                    _ => target = value,
                }
            }
            "jmp" => target = self.read(self.location(&a[0]), accesses)?,
            _ if i.is_conditional() => {
                let branch = match op {
                    "jrcxz" => self.state.registers[1] == 0,
                    "loop" | "loope" | "loopne" => {
                        self.state.registers[1] = self.state.registers[1].wrapping_sub(1);
                        self.state.registers[1] != 0
                            && match op {
                                "loope" => self.state.flags.zf,
                                "loopne" => !self.state.flags.zf,
                                _ => true,
                            }
                    }
                    _ => self.state.flags.condition(op),
                };
                taken = Some(branch);
                if branch {
                    target = i.direct_target().unwrap();
                }
            }
            _ if op.starts_with("cmov") => {
                let source = self.read(self.location(&a[1]), accesses)?;
                let dst = self.location(&a[0]);
                let value = if self.state.flags.condition(condition_code(op).unwrap()) {
                    source
                } else {
                    self.read(dst, accesses)?
                };
                self.write(dst, value, accesses)?;
            }
            _ if op.starts_with("set") => {
                let value = self.state.flags.condition(condition_code(op).unwrap());
                self.write(self.location(&a[0]), u64::from(value), accesses)?;
            }
            "movs" | "stos" | "rep.movs" | "rep.stos" => {
                let Operand::Imm(w, _) = a[0] else {
                    unreachable!()
                };
                let bytes = (w / 8) as u8;
                let repeated = op.starts_with("rep.");
                if !repeated || self.state.registers[1] != 0 {
                    let moving = op.ends_with("movs");
                    let value = if moving {
                        self.read(Location::Memory(self.state.registers[6], bytes), accesses)?
                    } else {
                        self.state.registers[0]
                    };
                    self.write(
                        Location::Memory(self.state.registers[7], bytes),
                        value,
                        accesses,
                    )?;
                    let delta = if self.state.flags.df {
                        0u64.wrapping_sub(bytes.into())
                    } else {
                        bytes.into()
                    };
                    if moving {
                        self.state.registers[6] = self.state.registers[6].wrapping_add(delta);
                    }
                    self.state.registers[7] = self.state.registers[7].wrapping_add(delta);
                    if repeated {
                        self.state.registers[1] -= 1;
                        if self.state.registers[1] != 0 {
                            target = i.address;
                        }
                    }
                }
            }
            _ => return Err(Stop::Unsupported(i.op.clone())),
        }
        self.state.rip = target;
        Ok(taken)
    }

    fn shift(
        &mut self,
        i: &Instruction,
        accesses: &mut Vec<Access>,
    ) -> std::result::Result<(), Stop> {
        let op = i.mnemonic();
        let a = &i.operands;
        let dst = self.location(&a[0]);
        let w = a[0].width();
        let original = self.read(dst, accesses)?;
        let mut value = original;
        let double = matches!(op, "shld" | "shrd");
        let mut source = if double {
            self.read(self.location(&a[1]), accesses)?
        } else {
            0
        };
        let count = self.read(self.location(&a[if double { 2 } else { 1 }]), accesses)?
            & if w == 64 { 63 } else { 31 };
        let through = matches!(op, "rcl" | "rcr");
        let iterations = if through && w < 32 {
            count % u64::from(w + 1)
        } else {
            count
        };
        if iterations != 0 {
            let overflow = self.state.flags.of;
            let mut carry = self.state.flags.cf;
            let sign = 1 << (w - 1);
            for _ in 0..iterations {
                let bit = if matches!(op, "shl" | "rol" | "rcl" | "shld") {
                    value & sign != 0
                } else {
                    value & 1 != 0
                };
                value = match op {
                    "shl" => value << 1,
                    "shr" => value >> 1,
                    "sar" => (value >> 1) | (value & sign),
                    "rol" => (value << 1) | u64::from(bit),
                    "ror" => (value >> 1) | (u64::from(bit) << (w - 1)),
                    "rcl" => (value << 1) | u64::from(carry),
                    "rcr" => (value >> 1) | (u64::from(carry) << (w - 1)),
                    "shld" => {
                        let v = (value << 1) | u64::from(source & sign != 0);
                        source = (source << 1) & mask(w);
                        v
                    }
                    _ => {
                        let v = (value >> 1) | ((source & 1) << (w - 1));
                        source >>= 1;
                        v
                    }
                } & mask(w);
                carry = bit;
            }
            if matches!(op, "shl" | "shr" | "sar" | "shld" | "shrd") {
                self.state.flags.result(value, w);
            }
            self.state.flags.cf = carry;
            self.state.flags.of = if count == 1 {
                match op {
                    "shl" | "rol" | "rcl" => (value & sign != 0) != carry,
                    "shr" => original & sign != 0,
                    "sar" => false,
                    "ror" | "rcr" => ((value >> (w - 1)) ^ (value >> (w - 2))) & 1 != 0,
                    _ => (original ^ value) & sign != 0,
                }
            } else {
                overflow
            };
        }
        self.write(dst, value, accesses)
    }

    fn multiply_divide(
        &mut self,
        i: &Instruction,
        accesses: &mut Vec<Access>,
    ) -> std::result::Result<(), Stop> {
        let op = i.mnemonic();
        let w = i.operands[0].width();
        let rhs = self.read(self.location(&i.operands[0]), accesses)?;
        let lo = self.state.registers[0] & mask(w);
        let hi = if w == 8 {
            (self.state.registers[0] >> 8) & 255
        } else {
            self.state.registers[2] & mask(w)
        };
        let (low, high) = if matches!(op, "mul" | "imul") {
            let product = if op == "imul" {
                (signed(lo, w) * signed(rhs, w)) as u128
            } else {
                lo as u128 * rhs as u128
            };
            let low = product as u64 & mask(w);
            let high = (product >> w) as u64 & mask(w);
            let overflow = if op == "imul" {
                high != if low & (1 << (w - 1)) != 0 {
                    mask(w)
                } else {
                    0
                }
            } else {
                high != 0
            };
            self.state.flags.cf = overflow;
            self.state.flags.of = overflow;
            (low, high)
        } else {
            let bits = ((hi as u128) << w) | lo as u128;
            let negative = op == "idiv" && hi & (1 << (w - 1)) != 0;
            let den_negative = op == "idiv" && rhs & (1 << (w - 1)) != 0;
            let magnitude = if negative {
                bits.wrapping_neg() & (u128::MAX >> (128 - 2 * u32::from(w)))
            } else {
                bits
            };
            let denominator = if den_negative {
                rhs.wrapping_neg() & mask(w)
            } else {
                rhs
            } as u128;
            if denominator == 0 {
                return Err(Stop::Trap("division by zero".into()));
            }
            let quotient = magnitude / denominator;
            let remainder = magnitude % denominator;
            let q_negative = negative != den_negative;
            let limit = if op == "idiv" {
                (1u128 << (w - 1)) - u128::from(!q_negative)
            } else {
                mask(w) as u128
            };
            if quotient > limit {
                return Err(Stop::Trap("division quotient overflow".into()));
            }
            let q = if q_negative {
                (quotient as u64).wrapping_neg()
            } else {
                quotient as u64
            };
            let r = if negative {
                (remainder as u64).wrapping_neg()
            } else {
                remainder as u64
            };
            (q & mask(w), r & mask(w))
        };
        if w == 8 {
            self.write(Location::Register(0, 16), (high << 8) | low, accesses)?;
        } else {
            self.write(Location::Register(0, w), low, accesses)?;
            self.write(Location::Register(2, w), high, accesses)?;
        }
        Ok(())
    }
}

fn mask(w: u8) -> u64 {
    u64::MAX >> (64 - w)
}
fn signed(value: u64, w: u8) -> i128 {
    ((value as i128) << (128 - w)) >> (128 - w)
}

#[derive(Clone, Copy)]
enum Location {
    Register(u8, u8),
    High(u8),
    Immediate(u64),
    Memory(u64, u8),
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Stop {
    LeftCode,
    Budget,
    Trap(String),
    MemoryFault(String),
    Unsupported(String),
}

impl Stop {
    pub fn name(&self) -> &'static str {
        match self {
            Self::LeftCode => "left-code",
            Self::Budget => "budget-exhausted",
            Self::Trap(_) => "trap",
            Self::MemoryFault(_) => "memory-fault",
            Self::Unsupported(_) => "unsupported-instruction",
        }
    }
    pub fn detail(&self) -> &str {
        match self {
            Self::Trap(s) | Self::MemoryFault(s) | Self::Unsupported(s) => s,
            _ => "",
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Access {
    pub address: u64,
    pub bytes: u8,
    pub value: u64,
    pub write: bool,
}

#[derive(Clone, Debug)]
pub struct Step {
    pub assembly: String,
    pub before: State,
    pub after: State,
    pub taken: Option<bool>,
    pub accesses: Vec<Access>,
    pub completed: bool,
}

#[derive(Clone, Copy, Debug)]
pub struct Options {
    pub max_steps: u64,
    pub trace_limit: usize,
}
impl Default for Options {
    fn default() -> Self {
        Self {
            max_steps: 10_000,
            trace_limit: 1_000,
        }
    }
}

#[derive(Clone, Debug)]
pub struct Report {
    pub initial: State,
    pub final_state: State,
    pub stop: Stop,
    /// Includes a faulting or unsupported instruction attempt.
    pub steps: u64,
    /// Execution attempts per instruction, including a faulting instruction.
    pub counts: BTreeMap<u64, u64>,
    /// Successfully executed transitions, including external targets.
    pub edges: BTreeMap<(u64, u64), u64>,
    pub instruction_count: usize,
    pub memory_reads: u64,
    pub memory_writes: u64,
    pub trace: Vec<Step>,
}

/// Execute from the machine's current RIP, allowing explicit resumption.
/// Faulting instructions retain their pre-instruction state and commit no stores.
pub fn execute(p: &Program, machine: &mut Machine, options: Options) -> Result<Report> {
    p.validate()?;
    if options.max_steps == 0 {
        return Err("step limit must be positive".into());
    }
    let instructions: BTreeMap<_, _> = p.instructions.iter().map(|i| (i.address, i)).collect();
    let mut r = Report {
        initial: machine.state,
        final_state: machine.state,
        stop: Stop::LeftCode,
        steps: 0,
        counts: BTreeMap::new(),
        edges: BTreeMap::new(),
        instruction_count: p.instructions.len(),
        memory_reads: 0,
        memory_writes: 0,
        trace: Vec::new(),
    };
    while let Some(i) = instructions.get(&machine.state.rip) {
        if r.steps == options.max_steps {
            r.stop = Stop::Budget;
            break;
        }
        let before = machine.state;
        let mut accesses = Vec::new();
        let result = machine.step(i, &mut accesses);
        r.steps += 1;
        *r.counts.entry(i.address).or_default() += 1;
        let completed = result.is_ok();
        if completed {
            for access in &accesses {
                if access.write {
                    // All writes were validated before any guest memory changes.
                    machine
                        .memory
                        .store(access.address, access.bytes, access.value)?;
                    r.memory_writes += 1;
                }
            }
            *r.edges.entry((before.rip, machine.state.rip)).or_default() += 1;
        } else {
            machine.state = before;
            // Successful reads remain observable; pending stores never happened.
            accesses.retain(|a| !a.write);
        }
        r.memory_reads += accesses.iter().filter(|a| !a.write).count() as u64;
        if r.trace.len() < options.trace_limit {
            r.trace.push(Step {
                assembly: listing(i),
                before,
                after: machine.state,
                taken: result.as_ref().ok().copied().flatten(),
                accesses,
                completed,
            });
        }
        if let Err(stop) = result {
            r.stop = stop;
            break;
        }
    }
    r.final_state = machine.state;
    Ok(r)
}

impl Report {
    pub fn json(&self) -> String {
        let counts = self
            .counts
            .iter()
            .map(|(address, count)| format!("{{\"address\":\"0x{address:x}\",\"count\":{count}}}"))
            .collect::<Vec<_>>()
            .join(",");
        let edges = self
            .edges
            .iter()
            .map(|((from, to), count)| {
                format!("{{\"from\":\"0x{from:x}\",\"to\":\"0x{to:x}\",\"count\":{count}}}")
            })
            .collect::<Vec<_>>()
            .join(",");
        let trace = self.trace.iter().map(|step| {
            let accesses = step.accesses.iter().map(|a| format!("{{\"kind\":\"{}\",\"address\":\"0x{:x}\",\"bytes\":{},\"value\":\"0x{:x}\"}}", if a.write { "write" } else { "read" }, a.address, a.bytes, a.value)).collect::<Vec<_>>().join(",");
            format!("{{\"assembly\":{},\"before\":{},\"after\":{},\"taken\":{},\"completed\":{},\"memory\":[{accesses}]}}", quoted(&step.assembly), step.before.json(), step.after.json(), step.taken.map_or("null", |v| if v { "true" } else { "false" }), step.completed)
        }).collect::<Vec<_>>().join(",");
        format!(
            "{{\"schema\":\"rosemary-trace-1\",\"stop\":{},\"detail\":{},\"steps\":{},\"initial\":{},\"final\":{},\"coverage\":{{\"visited\":{},\"total\":{}}},\"memory_reads\":{},\"memory_writes\":{},\"counts\":[{counts}],\"edges\":[{edges}],\"trace_truncated\":{},\"trace\":[{trace}]}}\n",
            quoted(self.stop.name()),
            quoted(self.stop.detail()),
            self.steps,
            self.initial.json(),
            self.final_state.json(),
            self.counts.len(),
            self.instruction_count,
            self.memory_reads,
            self.memory_writes,
            self.steps > self.trace.len() as u64
        )
    }

    pub fn text(&self) -> String {
        let mut s = format!(
            "dynamic execution: {} at 0x{:x}\n",
            self.stop.name(),
            self.final_state.rip
        );
        if !self.stop.detail().is_empty() {
            writeln!(s, "{}", self.stop.detail()).unwrap();
        }
        writeln!(
            s,
            "steps: {}; visited: {}/{} instructions; memory: {} reads, {} writes",
            self.steps,
            self.counts.len(),
            self.instruction_count,
            self.memory_reads,
            self.memory_writes
        )
        .unwrap();
        for step in &self.trace {
            write!(
                s,
                "0x{:x}: {} -> 0x{:x}",
                step.before.rip, step.assembly, step.after.rip
            )
            .unwrap();
            if let Some(taken) = step.taken {
                write!(s, " ({})", if taken { "taken" } else { "fallthrough" }).unwrap();
            }
            if !step.completed {
                s.push_str(" (stopped)");
            }
            s.push('\n');
            for (n, name) in REGISTERS.iter().enumerate() {
                if step.before.registers[n] != step.after.registers[n] {
                    writeln!(
                        s,
                        "  {name}: 0x{:x} -> 0x{:x}",
                        step.before.registers[n], step.after.registers[n]
                    )
                    .unwrap();
                }
            }
            if step.before.flags != step.after.flags {
                writeln!(s, "  flags: {}", step.after.flags.json()).unwrap();
            }
            for a in &step.accesses {
                writeln!(
                    s,
                    "  {}{} [0x{:x}] = 0x{:x}",
                    if a.write { "write" } else { "read" },
                    a.bytes * 8,
                    a.address,
                    a.value
                )
                .unwrap();
            }
        }
        if self.steps > self.trace.len() as u64 {
            s.push_str("trace truncated; totals include all steps\n");
        }
        s.push_str("final registers:\n");
        for (name, value) in REGISTERS.iter().zip(self.final_state.registers) {
            writeln!(s, "  {name}=0x{value:x}").unwrap();
        }
        writeln!(s, "final flags: {}", self.final_state.flags.json()).unwrap();
        s
    }
}
