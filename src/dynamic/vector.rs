//! Concrete bit-vector execution, independent of host SIMD features.
use super::{Access, Location, Machine, Stop, mask};
use crate::ir::{Instruction, Operand};

impl Machine {
    fn vector_read(&self, operand: &Operand, accesses: &mut Vec<Access>) -> Result<u128, Stop> {
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
        if !matches!(
            op,
            "movups"
                | "movaps"
                | "movupd"
                | "movapd"
                | "movdqu"
                | "movdqa"
                | "movd"
                | "movq"
                | "movss"
                | "movsd"
                | "movlps"
                | "movlpd"
                | "movhps"
                | "movhpd"
                | "movddup"
                | "movsldup"
                | "movshdup"
                | "pmovmskb"
                | "movmskps"
                | "movmskpd"
                | "pextrw"
                | "pinsrw"
                | "pxor"
                | "xorps"
                | "xorpd"
                | "pand"
                | "andps"
                | "andpd"
                | "pandn"
                | "andnps"
                | "andnpd"
                | "por"
                | "orps"
                | "orpd"
                | "movlhps"
                | "movhlps"
        ) {
            return Err(Stop::Unsupported(i.op.clone()));
        }
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
            _ => return Err(Stop::Unsupported(i.op.clone())),
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
