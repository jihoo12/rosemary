//! Concrete bit-vector execution, independent of host SIMD features.
use super::{Access, Location, Machine, Stop, mask, signed};
use crate::ir::{Instruction, Operand};

impl Machine {
    pub(super) fn vector_read(
        &self,
        operand: &Operand,
        accesses: &mut Vec<Access>,
    ) -> Result<u128, Stop> {
        match operand {
            Operand::Vec(r) => Ok(self.state.xmm[*r as usize]),
            Operand::Mem { width: 128, .. } => {
                let address = self.address(operand);
                let lo = self.read(Location::Memory(address, 8), accesses)?;
                let high = address
                    .checked_add(8)
                    .ok_or_else(|| Stop::MemoryFault("memory access overflow".into()))?;
                let hi = self.read(Location::Memory(high, 8), accesses)?;
                Ok(u128::from(lo) | (u128::from(hi) << 64))
            }
            _ => self.read(self.location(operand), accesses).map(u128::from),
        }
    }

    fn vector_write(
        &mut self,
        operand: &Operand,
        value: u128,
        accesses: &mut Vec<Access>,
    ) -> Result<(), Stop> {
        match operand {
            Operand::Vec(r) => self.state.xmm[*r as usize] = value,
            Operand::Mem { width: 128, .. } => {
                let address = self.address(operand);
                self.write(Location::Memory(address, 8), value as u64, accesses)?;
                let high = address
                    .checked_add(8)
                    .ok_or_else(|| Stop::MemoryFault("memory access overflow".into()))?;
                self.write(Location::Memory(high, 8), (value >> 64) as u64, accesses)?;
            }
            _ => self.write(self.location(operand), value as u64, accesses)?,
        }
        Ok(())
    }

    pub(super) fn vector_step(
        &mut self,
        i: &Instruction,
        accesses: &mut Vec<Access>,
    ) -> Result<(), Stop> {
        let a = &i.operands;
        let op = i.mnemonic();
        let source = self.vector_read(&a[1], accesses)?;
        let value = match op {
            "movups" | "movaps" | "movupd" | "movapd" | "movdqu" | "movdqa" => source,
            "movd" | "movq" | "movss" | "movsd" => {
                let bits = if matches!(op, "movd" | "movss") {
                    32
                } else {
                    64
                };
                let keep = if matches!(op, "movss" | "movsd")
                    && matches!((&a[0], &a[1]), (Operand::Vec(_), Operand::Vec(_)))
                {
                    self.vector_read(&a[0], accesses)? & !u128::from(mask(bits))
                } else {
                    0
                };
                keep | (source & u128::from(mask(bits)))
            }
            "movlps" | "movlpd" | "movhps" | "movhpd" => {
                let shift = if matches!(op, "movhps" | "movhpd") {
                    64
                } else {
                    0
                };
                if matches!(a[0], Operand::Vec(_)) {
                    let old = self.vector_read(&a[0], accesses)?;
                    (old & !(u128::from(u64::MAX) << shift)) | (source << shift)
                } else {
                    source >> shift
                }
            }
            "movddup" => (source as u64 as u128) * ((1u128 << 64) | 1),
            "movsldup" | "movshdup" => {
                let offset = usize::from(op == "movshdup");
                let mut value = 0;
                for n in 0..4 {
                    set_lane(&mut value, n, 32, lane(source, (n & 2) + offset, 32));
                }
                value
            }
            "pmovmskb" | "movmskps" | "movmskpd" => {
                let bits = match op {
                    "pmovmskb" => 8,
                    "movmskps" => 32,
                    _ => 64,
                };
                let mut value = 0;
                for n in 0..128 / usize::from(bits) {
                    value |= u128::from(lane(source, n, bits) >> (bits - 1)) << n;
                }
                value
            }
            "pextrw" => {
                let index = self.read(self.location(&a[2]), accesses)? as usize & 7;
                u128::from(lane(source, index, 16))
            }
            "pinsrw" => {
                let index = self.read(self.location(&a[2]), accesses)? as usize & 7;
                let mut value = self.vector_read(&a[0], accesses)?;
                set_lane(&mut value, index, 16, source as u64);
                value
            }
            "pxor" | "xorps" | "xorpd" => self.vector_read(&a[0], accesses)? ^ source,
            "pand" | "andps" | "andpd" => self.vector_read(&a[0], accesses)? & source,
            "pandn" | "andnps" | "andnpd" => !self.vector_read(&a[0], accesses)? & source,
            "por" | "orps" | "orpd" => self.vector_read(&a[0], accesses)? | source,
            "movlhps" => {
                (self.vector_read(&a[0], accesses)? & u128::from(u64::MAX)) | (source << 64)
            }
            "movhlps" => (self.vector_read(&a[0], accesses)? & (u128::MAX << 64)) | (source >> 64),
            "palignr" => {
                let count = self.read(self.location(&a[2]), accesses)? as usize;
                let left = self.vector_read(&a[0], accesses)?;
                let mut value = 0;
                for n in 0..16 {
                    let index = n + count;
                    let byte = if index < 16 {
                        lane(source, index, 8)
                    } else if index < 32 {
                        lane(left, index - 16, 8)
                    } else {
                        0
                    };
                    set_lane(&mut value, n, 8, byte);
                }
                value
            }
            "pshufd" | "pshuflw" | "pshufhw" => {
                let select = self.read(self.location(&a[2]), accesses)?;
                let bits = if op == "pshufd" { 32 } else { 16 };
                let offset = if op == "pshufhw" { 4 } else { 0 };
                let mut value = source;
                for n in 0..4 {
                    let index = ((select >> (2 * n)) & 3) as usize + offset;
                    set_lane(&mut value, n + offset, bits, lane(source, index, bits));
                }
                value
            }
            "shufps" | "shufpd" => {
                let select = self.read(self.location(&a[2]), accesses)?;
                let left = self.vector_read(&a[0], accesses)?;
                let (bits, stride) = if op == "shufps" { (32, 2) } else { (64, 1) };
                let count = 128 / usize::from(bits);
                let mut value = 0;
                for n in 0..count {
                    let index = ((select >> (stride * n)) as usize) & (count - 1);
                    let operand = if n < count / 2 { left } else { source };
                    set_lane(&mut value, n, bits, lane(operand, index, bits));
                }
                value
            }
            "pslldq" | "psrldq" => {
                let left = self.vector_read(&a[0], accesses)?;
                if source >= 16 {
                    0
                } else if op == "pslldq" {
                    left << (source * 8)
                } else {
                    left >> (source * 8)
                }
            }
            _ => {
                let left = self.vector_read(&a[0], accesses)?;
                packed(op, left, source)
            }
        };
        self.vector_write(&a[0], value, accesses)
    }
}

fn lane(value: u128, n: usize, bits: u8) -> u64 {
    (value >> (n * usize::from(bits))) as u64 & mask(bits)
}

fn set_lane(value: &mut u128, n: usize, bits: u8, lane: u64) {
    let shift = n * usize::from(bits);
    *value =
        (*value & !(u128::from(mask(bits)) << shift)) | (u128::from(lane & mask(bits)) << shift);
}

fn packed(op: &str, left: u128, right: u128) -> u128 {
    let mut value = 0;
    if let Some((bits, kind)) = crate::vector::lanes(op) {
        for n in 0..128 / usize::from(bits) {
            let a = lane(left, n, bits);
            let b = lane(right, n, bits);
            let sa = signed(a, bits);
            let sb = signed(b, bits);
            let result = match kind {
                "psign" => {
                    if sb < 0 {
                        a.wrapping_neg()
                    } else if b == 0 {
                        0
                    } else {
                        a
                    }
                }
                "pabs" => {
                    if sb < 0 {
                        b.wrapping_neg()
                    } else {
                        b
                    }
                }
                "padd" => a.wrapping_add(b),
                "psub" => a.wrapping_sub(b),
                "paddus" => (a + b).min(mask(bits)),
                "psubus" => a.saturating_sub(b),
                "padds" | "psubs" => {
                    let sign = 1i128 << (bits - 1);
                    let sum = if kind == "padds" { sa + sb } else { sa - sb };
                    sum.clamp(-sign, sign - 1) as u64
                }
                "pavg" => (a + b + 1) >> 1,
                "pmull" => a * b,
                "pmulhu" => (a * b) >> 16,
                "pmulh" => ((sa * sb) >> 16) as u64,
                "pmulhrs" => ((sa * sb + 0x4000) >> 15) as u64,
                "pcmpeq" => {
                    if a == b {
                        mask(bits)
                    } else {
                        0
                    }
                }
                "pcmpgt" => {
                    if sa > sb {
                        mask(bits)
                    } else {
                        0
                    }
                }
                "pminu" => a.min(b),
                "pmaxu" => a.max(b),
                "pmins" => sa.min(sb) as u64,
                "pmaxs" => sa.max(sb) as u64,
                "psll" | "psrl" | "psra" => {
                    // The complete low qword supplies the count, without masking.
                    let count = right as u64;
                    if count >= u64::from(bits) {
                        if kind == "psra" && sa < 0 {
                            mask(bits)
                        } else {
                            0
                        }
                    } else {
                        match kind {
                            "psll" => a << count,
                            "psrl" => a >> count,
                            _ => (sa >> count) as u64,
                        }
                    }
                }
                _ => unreachable!("recognized packed lane operation"),
            };
            set_lane(&mut value, n, bits, result);
        }
    } else if let Some((bits, subtract, saturating)) = crate::vector::horizontal(op) {
        let half = 64 / usize::from(bits);
        for n in 0..2 * half {
            let source = if n < half { left } else { right };
            let index = 2 * (n % half);
            let a = lane(source, index, bits);
            let b = lane(source, index + 1, bits);
            let result = if saturating {
                let a = signed(a, bits);
                let b = signed(b, bits);
                (if subtract { a - b } else { a + b }).clamp(-32768, 32767) as u64
            } else if subtract {
                a.wrapping_sub(b)
            } else {
                a.wrapping_add(b)
            };
            set_lane(&mut value, n, bits, result);
        }
    } else if let Some((bits, high)) = crate::vector::unpack(op) {
        let half = 64 / usize::from(bits);
        let offset = if high { half } else { 0 };
        for n in 0..half {
            set_lane(&mut value, 2 * n, bits, lane(left, n + offset, bits));
            set_lane(&mut value, 2 * n + 1, bits, lane(right, n + offset, bits));
        }
    } else if let Some((bits, unsigned)) = crate::vector::pack(op) {
        let out = bits / 2;
        let low = if unsigned { 0 } else { -(1i128 << (out - 1)) };
        let high = (1i128 << (out - u8::from(!unsigned))) - 1;
        let half = 128 / usize::from(bits);
        for n in 0..2 * half {
            let source = if n < half { left } else { right };
            let result = signed(lane(source, n % half, bits), bits).clamp(low, high);
            set_lane(&mut value, n, out, result as u64);
        }
    } else {
        match op {
            "pmuldq" => {
                for n in 0..2 {
                    let product =
                        signed(lane(left, 2 * n, 32), 32) * signed(lane(right, 2 * n, 32), 32);
                    set_lane(&mut value, n, 64, product as u64);
                }
            }
            "pmuludq" => {
                for n in 0..2 {
                    let product = lane(left, 2 * n, 32) * lane(right, 2 * n, 32);
                    set_lane(&mut value, n, 64, product);
                }
            }
            "pmaddwd" => {
                for n in 0..4 {
                    let mut sum = 0i128;
                    for k in 0..2 {
                        sum += signed(lane(left, 2 * n + k, 16), 16)
                            * signed(lane(right, 2 * n + k, 16), 16);
                    }
                    set_lane(&mut value, n, 32, sum as u64);
                }
            }
            "pmaddubsw" => {
                for n in 0..8 {
                    let mut sum = 0i128;
                    for k in 0..2 {
                        sum += i128::from(lane(left, 2 * n + k, 8))
                            * signed(lane(right, 2 * n + k, 8), 8);
                    }
                    set_lane(&mut value, n, 16, sum.clamp(-32768, 32767) as u64);
                }
            }
            "psadbw" => {
                for n in 0..2 {
                    let sum = (0..8)
                        .map(|k| lane(left, 8 * n + k, 8).abs_diff(lane(right, 8 * n + k, 8)))
                        .sum();
                    set_lane(&mut value, n, 64, sum);
                }
            }
            "pshufb" => {
                for n in 0..16 {
                    let select = lane(right, n, 8);
                    let byte = if select & 128 != 0 {
                        0
                    } else {
                        lane(left, (select & 15) as usize, 8)
                    };
                    set_lane(&mut value, n, 8, byte);
                }
            }
            _ => unreachable!("recognized vector operation"),
        }
    }
    value
}
