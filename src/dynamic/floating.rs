//! IEEE bit-pattern comparison with guest MXCSR, independent of host FP state.
use super::{Access, Machine, Stop, mask};
use crate::ir::Instruction;

impl Machine {
    pub(super) fn floating_step(
        &mut self,
        i: &Instruction,
        accesses: &mut Vec<Access>,
    ) -> Result<(), Stop> {
        let bits = if i.op.ends_with("ss") { 32 } else { 64 };
        let mut left = self.vector_read(&i.operands[0], accesses)? as u64 & mask(bits);
        let mut right = self.vector_read(&i.operands[1], accesses)? as u64 & mask(bits);
        let sign = 1u64 << (bits - 1);
        let fraction = (1u64 << if bits == 32 { 23 } else { 52 }) - 1;
        let exponent = (sign - 1) ^ fraction;
        let quiet = (fraction + 1) >> 1;
        let nan = |v: u64| v & exponent == exponent && v & fraction != 0;
        let nan_left = nan(left);
        let nan_right = nan(right);
        let unordered = nan_left || nan_right;
        let mut exception = 0;
        if unordered {
            if i.op.starts_with("comi")
                || (nan_left && left & quiet == 0)
                || (nan_right && right & quiet == 0)
            {
                exception = 1;
            }
        } else {
            let denormal = |v: u64| v & exponent == 0 && v & fraction != 0;
            let denormal_left = denormal(left);
            let denormal_right = denormal(right);
            if self.state.mxcsr & 64 != 0 {
                if denormal_left {
                    left &= sign;
                }
                if denormal_right {
                    right &= sign;
                }
            } else if denormal_left || denormal_right {
                exception = 2;
            }
        }
        self.state.mxcsr |= exception;
        if exception & !(self.state.mxcsr >> 7) != 0 {
            return Err(Stop::SimdException(exception as u8));
        }
        let magnitude_left = left & (sign - 1);
        let magnitude_right = right & (sign - 1);
        let equal = left == right || (magnitude_left == 0 && magnitude_right == 0);
        let less = if (left ^ right) & sign != 0 {
            left & sign != 0
        } else if left & sign != 0 {
            magnitude_left > magnitude_right
        } else {
            magnitude_left < magnitude_right
        };
        self.state.flags.zf = unordered || equal;
        self.state.flags.pf = unordered;
        self.state.flags.cf = unordered || (!equal && less);
        self.state.flags.of = false;
        self.state.flags.sf = false;
        Ok(())
    }
}
