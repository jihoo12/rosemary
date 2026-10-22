//! Monotone constant propagation over machine state, with conservative calls.
use crate::{
    Result,
    analysis::{hex, listing, quoted},
    emit::asm_operand,
    ir::{Instruction, Operand, Program, condition_code},
};
use std::{
    collections::{BTreeMap, VecDeque},
    fmt::Write,
};

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Flags {
    pub zf: Option<bool>,
    pub sf: Option<bool>,
    pub cf: Option<bool>,
    pub of: Option<bool>,
    pub pf: Option<bool>,
}
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct State {
    pub registers: [Option<u64>; 16],
    pub flags: Flags,
    pub direction: Option<bool>,
}

fn mask(width: u8) -> u64 {
    if width == 64 {
        u64::MAX
    } else {
        (1u64 << width) - 1
    }
}
fn and(a: Option<bool>, b: Option<bool>) -> Option<bool> {
    if a == Some(false) || b == Some(false) {
        Some(false)
    } else {
        a.zip(b).map(|(a, b)| a && b)
    }
}
fn or(a: Option<bool>, b: Option<bool>) -> Option<bool> {
    if a == Some(true) || b == Some(true) {
        Some(true)
    } else {
        a.zip(b).map(|(a, b)| a || b)
    }
}
fn not(a: Option<bool>) -> Option<bool> {
    a.map(|a| !a)
}
impl Flags {
    fn on_condition(self, op: &str, taken: bool) -> Option<Self> {
        // Keep only independent flag constants shared by every concrete state
        // compatible with this edge. Relations such as SF != OF are not stored.
        let mut result: Option<Self> = None;
        for bits in 0..32 {
            let values: [bool; 5] = std::array::from_fn(|n| bits & (1 << n) != 0);
            if self
                .entries()
                .into_iter()
                .zip(values)
                .any(|((_, known), value)| known.is_some_and(|known| known != value))
            {
                continue;
            }
            let [zf, sf, cf, of, pf] = values.map(Some);
            let candidate = Self { zf, sf, cf, of, pf };
            if candidate.condition(op) == Some(taken) {
                if let Some(common) = &mut result {
                    common.join(candidate);
                } else {
                    result = Some(candidate);
                }
            }
        }
        result
    }

    pub fn condition(&self, op: &str) -> Option<bool> {
        let less = self.sf.zip(self.of).map(|(s, o)| s != o);
        match op {
            "jo" => self.of,
            "jno" => not(self.of),
            "jb" => self.cf,
            "jae" => not(self.cf),
            "je" => self.zf,
            "jne" => not(self.zf),
            "jbe" => or(self.cf, self.zf),
            "ja" => and(not(self.cf), not(self.zf)),
            "js" => self.sf,
            "jns" => not(self.sf),
            "jp" => self.pf,
            "jnp" => not(self.pf),
            "jl" => less,
            "jge" => not(less),
            "jle" => or(self.zf, less),
            "jg" => and(not(self.zf), not(less)),
            _ => None,
        }
    }
    fn entries(self) -> [(&'static str, Option<bool>); 5] {
        [
            ("ZF", self.zf),
            ("SF", self.sf),
            ("CF", self.cf),
            ("OF", self.of),
            ("PF", self.pf),
        ]
    }
    fn join(&mut self, other: Self) {
        if self.zf != other.zf {
            self.zf = None;
        }
        if self.sf != other.sf {
            self.sf = None;
        }
        if self.cf != other.cf {
            self.cf = None;
        }
        if self.of != other.of {
            self.of = None;
        }
        if self.pf != other.pf {
            self.pf = None;
        }
    }
}
impl State {
    fn branch_condition(&self, i: &Instruction) -> Option<bool> {
        if i.is_counter_loop() {
            let nonzero = self.registers[1].map(|v| v.wrapping_sub(1) != 0);
            match i.op.as_str() {
                "loope" => and(nonzero, self.flags.zf),
                "loopne" => and(nonzero, not(self.flags.zf)),
                _ => nonzero,
            }
        } else if i.op == "jrcxz" {
            self.registers[1].map(|v| v == 0)
        } else {
            self.flags.condition(&i.op)
        }
    }

    /// Refine a feasible conditional edge after applying instruction effects.
    fn on_branch(mut self, i: &Instruction, taken: bool) -> Option<Self> {
        if i.op == "jrcxz" {
            if self.registers[1].is_some_and(|v| (v == 0) != taken) {
                return None;
            }
            if taken {
                self.registers[1] = Some(0);
            }
        } else if i.is_counter_loop() {
            let zero = self.registers[1].map(|v| v == 0);
            if i.op == "loop" {
                if zero == Some(taken) {
                    return None;
                }
                if !taken {
                    self.registers[1] = Some(0);
                }
            } else {
                let want_zf = i.op == "loope";
                if taken {
                    if zero == Some(true) || self.flags.zf == Some(!want_zf) {
                        return None;
                    }
                    self.flags.zf = Some(want_zf);
                } else if zero == Some(false) {
                    if self.flags.zf == Some(want_zf) {
                        return None;
                    }
                    self.flags.zf = Some(!want_zf);
                } else if self.flags.zf == Some(want_zf) {
                    self.registers[1] = Some(0);
                }
            }
        } else {
            self.flags = self.flags.on_condition(&i.op, taken)?;
        }
        Some(self)
    }

    pub fn read(&self, operand: &Operand) -> Option<u64> {
        match operand {
            Operand::Imm(v, _) => Some(*v),
            Operand::Vec(_) => None,
            Operand::High(r) => self.registers[*r as usize].map(|v| (v >> 8) & 255),
            Operand::Reg(r, w) => self.registers[*r as usize].map(|v| v & mask(*w)),
            Operand::Mem { .. } => None, // no assumptions about guest memory or aliases
        }
    }
    pub fn address(&self, operand: &Operand) -> Option<u64> {
        let Operand::Mem {
            segment,
            base,
            index,
            scale,
            disp,
            ..
        } = operand
        else {
            return None;
        };
        if segment.is_some() {
            return None;
        }
        let b = if let Some(r) = base {
            self.registers[*r as usize]?
        } else {
            0
        };
        let i = if let Some(r) = index {
            self.registers[*r as usize]?
        } else {
            0
        };
        Some(
            disp.wrapping_add(b)
                .wrapping_add(i.wrapping_mul(*scale as u64)),
        )
    }
    pub(crate) fn write(&mut self, operand: &Operand, value: Option<u64>) {
        if let Operand::High(r) = operand {
            self.registers[*r as usize] = self.registers[*r as usize]
                .zip(value)
                .map(|(old, v)| (old & !0xff00) | ((v & 255) << 8));
            return;
        }
        if let Operand::Reg(r, w) = operand {
            self.registers[*r as usize] = if *w < 32 {
                self.registers[*r as usize]
                    .zip(value)
                    .map(|(old, v)| (old & !mask(*w)) | (v & mask(*w)))
            } else {
                value.map(|v| v & mask(*w))
            };
        }
    }
    fn join(&mut self, other: Self) -> bool {
        let before = *self;
        for (a, b) in self.registers.iter_mut().zip(other.registers) {
            if *a != b {
                *a = None;
            }
        }
        self.flags.join(other.flags);
        if self.direction != other.direction {
            self.direction = None;
        }
        *self != before
    }
}
pub(crate) fn result_flags(value: Option<u64>, width: u8) -> Flags {
    Flags {
        zf: value.map(|v| v == 0),
        sf: value.map(|v| v & (1 << (width - 1)) != 0),
        pf: value.map(|v| (v as u8).count_ones().is_multiple_of(2)),
        ..Default::default()
    }
}
pub(crate) fn arithmetic_flags(
    a: Option<u64>,
    b: Option<u64>,
    value: Option<u64>,
    width: u8,
    sub: bool,
) -> Flags {
    let mut flags = result_flags(value, width);
    if let Some(((a, b), v)) = a.zip(b).zip(value) {
        let sign = 1 << (width - 1);
        flags.cf = Some(if sub { a < b } else { v < a });
        flags.of = Some(if sub {
            (a ^ b) & (a ^ v) & sign != 0
        } else {
            !(a ^ b) & (a ^ v) & sign != 0
        });
    }
    flags
}
fn same_register(a: &Operand, b: &Operand) -> bool {
    matches!(a, Operand::Reg(..) | Operand::High(_)) && a == b
}
fn signed(v: u64, width: u8) -> i128 {
    ((v << (64 - width)) as i64 >> (64 - width)) as i128
}

/// Architectural undefined flags are forgotten, even when the C backend chooses
/// a deterministic value for them.
/// The instruction must belong to a validated Program.
pub fn transfer(instruction: &Instruction, mut state: State) -> State {
    let unprefixed = instruction.without_prefix();
    let instruction = unprefixed.as_ref();
    let a = &instruction.operands;
    match instruction.op.as_str() {
        _ if crate::strings::valid(instruction).is_some() => {
            return crate::strings::transfer(instruction, state);
        }
        _ if crate::integer::valid(instruction).is_some() => {
            return crate::integer::transfer(instruction, state);
        }
        _ if crate::floating::valid(instruction).is_some() => {
            // Facts describe successful continuations; XMM values and MXCSR
            // are not tracked, so the three comparison outcomes stay unknown.
            state.flags = Flags {
                sf: Some(false),
                of: Some(false),
                ..Flags::default()
            };
        }
        op if crate::vector::recognized(op) => {
            if let Some(dst @ Operand::Reg(..)) = a.first() {
                state.write(dst, None);
            }
        }
        "mov" | "movsxd" | "lea" | "movsx" | "movzx" => {
            let value = match instruction.op.as_str() {
                "lea" => state.address(&a[1]),
                "movsxd" | "movsx" => state.read(&a[1]).map(|v| {
                    let sign = 1u64 << (a[1].width() - 1);
                    (v ^ sign).wrapping_sub(sign)
                }),
                _ => state.read(&a[1]),
            };
            state.write(&a[0], value);
        }
        "add" | "sub" | "cmp" | "xor" | "and" | "or" | "test" => {
            let w = a[0].width();
            let mut left = state.read(&a[0]);
            let mut right = state.read(&a[1]);
            if same_register(&a[0], &a[1])
                && matches!(instruction.op.as_str(), "sub" | "cmp" | "xor")
            {
                left = Some(0);
                right = Some(0);
            }
            let value = if matches!(instruction.op.as_str(), "and" | "test")
                && (left == Some(0) || right == Some(0))
            {
                Some(0)
            } else if instruction.op == "or" && (left == Some(mask(w)) || right == Some(mask(w))) {
                Some(mask(w))
            } else {
                left.zip(right).map(|(a, b)| {
                    (match instruction.op.as_str() {
                        "add" => a.wrapping_add(b),
                        "sub" | "cmp" => a.wrapping_sub(b),
                        "xor" => a ^ b,
                        "or" => a | b,
                        _ => a & b,
                    }) & mask(w)
                })
            };
            state.flags = match instruction.op.as_str() {
                "add" => arithmetic_flags(left, right, value, w, false),
                "sub" | "cmp" => arithmetic_flags(left, right, value, w, true),
                _ => Flags {
                    cf: Some(false),
                    of: Some(false),
                    ..result_flags(value, w)
                },
            };
            if !matches!(instruction.op.as_str(), "cmp" | "test") {
                state.write(&a[0], value);
            }
        }
        "inc" | "dec" | "neg" | "not" => {
            let w = a[0].width();
            let left = state.read(&a[0]);
            let value = left.map(|v| {
                (match instruction.op.as_str() {
                    "inc" => v.wrapping_add(1),
                    "dec" => v.wrapping_sub(1),
                    "neg" => 0u64.wrapping_sub(v),
                    _ => !v,
                }) & mask(w)
            });
            let carry = state.flags.cf;
            match instruction.op.as_str() {
                "inc" | "dec" => {
                    state.flags =
                        arithmetic_flags(left, Some(1), value, w, instruction.op == "dec");
                    state.flags.cf = carry;
                }
                "neg" => state.flags = arithmetic_flags(Some(0), left, value, w, true),
                _ => {}
            }
            state.write(&a[0], value);
        }
        "imul" => {
            let w = a[0].width();
            let (left, right) = if a.len() == 3 {
                (state.read(&a[1]), state.read(&a[2]))
            } else {
                (state.read(&a[0]), state.read(&a[1]))
            };
            let (value, overflow) = if left == Some(0) || right == Some(0) {
                (Some(0), Some(false))
            } else if let Some((a, b)) = left.zip(right) {
                let v = a.wrapping_mul(b) & mask(w);
                (Some(v), Some(signed(v, w) != signed(a, w) * signed(b, w)))
            } else {
                (None, None)
            };
            state.write(&a[0], value);
            state.flags = Flags {
                cf: overflow,
                of: overflow,
                ..Default::default()
            };
        }
        "shl" | "shr" | "sar" => {
            let w = a[0].width();
            let value = state.read(&a[0]);
            let count = state.read(&a[1]).map(|v| v & if w == 64 { 63 } else { 31 });
            match count {
                Some(0) => state.write(&a[0], value),
                Some(count) => {
                    let sign = 1u64 << (w - 1);
                    let mut carry = None;
                    let mut result = value;
                    for _ in 0..count {
                        carry = result.map(|v| {
                            if instruction.op == "shl" {
                                v & sign != 0
                            } else {
                                v & 1 != 0
                            }
                        });
                        result = result.map(|v| match instruction.op.as_str() {
                            "shl" => (v << 1) & mask(w),
                            "shr" => v >> 1,
                            _ => (v >> 1) | (v & sign),
                        });
                    }
                    if count >= w as u64 && instruction.op != "sar" {
                        result = Some(0);
                        carry = None;
                    }
                    state.flags = result_flags(result, w);
                    state.flags.cf = carry;
                    state.flags.of = if count == 1 {
                        match instruction.op.as_str() {
                            "shl" => result.zip(carry).map(|(v, c)| (v & sign != 0) != c),
                            "shr" => value.map(|v| v & sign != 0),
                            _ => Some(false),
                        }
                    } else {
                        None
                    };
                    state.write(&a[0], result);
                }
                None => {
                    state.write(&a[0], None);
                    state.flags = Flags::default();
                }
            }
        }
        op if op.starts_with("cmov") => {
            let old = state.read(&a[0]);
            let source = state.read(&a[1]);
            let value = match state.flags.condition(condition_code(op).unwrap()) {
                Some(true) => source,
                Some(false) => old,
                None if old == source => old,
                _ => None,
            };
            state.write(&a[0], value);
        }
        op if op.starts_with("set") => {
            let value = state
                .flags
                .condition(condition_code(op).unwrap())
                .map(u64::from);
            state.write(&a[0], value);
        }
        _ if instruction.is_counter_loop() => {
            state.registers[1] = state.registers[1].map(|v| v.wrapping_sub(1));
        }
        "push" => state.registers[4] = state.registers[4].map(|v| v.wrapping_sub(8)),
        "pop" => {
            state.registers[4] = state.registers[4].map(|v| v.wrapping_add(8));
            state.write(&a[0], None);
        }
        "leave" => {
            state.registers[4] = state.registers[5].map(|v| v.wrapping_add(8));
            state.registers[5] = None;
        }
        "call" => state = State::default(), // no ABI or callee-return assumptions about values
        "ret" => {
            state.registers[4] =
                state.registers[4].map(|v| v.wrapping_add(instruction.return_stack_bytes()));
        }
        _ => {}
    }
    state
}

#[derive(Debug)]
pub struct ValueAnalysis<'a> {
    pub program: &'a Program,
    /// None is unreachable; a present State can contain unknown values.
    pub states: Vec<Option<State>>,
    pub iterations: usize,
    /// A reachable unknown indirect transfer forces unknown input at every
    /// decoded instruction, including instructions in the middle of CFG blocks.
    pub conservative_indirect: bool,
}
fn propagate(
    index: usize,
    state: State,
    states: &mut [Option<State>],
    queued: &mut [bool],
    pending: &mut VecDeque<usize>,
) {
    let changed = if let Some(old) = &mut states[index] {
        old.join(state)
    } else {
        states[index] = Some(state);
        true
    };
    if changed && !queued[index] {
        queued[index] = true;
        pending.push_back(index);
    }
}
pub fn analyze(program: &Program) -> Result<ValueAnalysis<'_>> {
    program.validate()?;
    let indices: BTreeMap<_, _> = program
        .instructions
        .iter()
        .enumerate()
        .map(|(n, i)| (i.address, n))
        .collect();
    let mut states = vec![None; program.instructions.len()];
    let mut queued = vec![false; states.len()];
    let mut pending = VecDeque::new();
    propagate(
        indices[&program.entry],
        State::default(),
        &mut states,
        &mut queued,
        &mut pending,
    );
    let mut iterations = 0;
    let mut conservative_indirect = false;
    while let Some(n) = pending.pop_front() {
        queued[n] = false;
        iterations += 1;
        let i = &program.instructions[n];
        let before = states[n].unwrap();
        let after = transfer(i, before);
        let conditional = i.is_conditional();
        let condition = before.branch_condition(i);
        if i.op.starts_with("rep.") {
            if after.registers[1] != Some(0) {
                propagate(n, after, &mut states, &mut queued, &mut pending);
            }
            if (after.registers[1].is_none() || after.registers[1] == Some(0))
                && let Some(&next) = indices.get(&(i.address + i.len as u64))
            {
                let mut finished = after;
                finished.registers[1] = Some(0);
                propagate(next, finished, &mut states, &mut queued, &mut pending);
            }
            continue;
        }
        if matches!(i.mnemonic(), "jmp" | "call") || conditional {
            let target = before.read(&i.operands[0]);
            if target.is_none() && !conservative_indirect {
                conservative_indirect = true;
                for index in 0..states.len() {
                    propagate(
                        index,
                        State::default(),
                        &mut states,
                        &mut queued,
                        &mut pending,
                    );
                }
            }
            if condition != Some(false)
                && let Some(index) = target.and_then(|a| indices.get(&a)).copied()
            {
                // Analyze callees independently of their callers and discard
                // possible stack/register correlations across calls.
                let state = if i.mnemonic() == "call" {
                    State::default()
                } else {
                    after
                };
                let state = if conditional {
                    state.on_branch(i, true)
                } else {
                    Some(state)
                };
                if let Some(state) = state {
                    propagate(index, state, &mut states, &mut queued, &mut pending);
                }
            }
        }
        if i.falls_through()
            && (!conditional || condition != Some(true))
            && let Some(&next) = indices.get(&(i.address + i.len as u64))
        {
            let state = if conditional {
                after.on_branch(i, false)
            } else {
                Some(after)
            };
            if let Some(state) = state {
                propagate(next, state, &mut states, &mut queued, &mut pending);
            }
        }
    }
    Ok(ValueAnalysis {
        program,
        states,
        iterations,
        conservative_indirect,
    })
}

impl ValueAnalysis<'_> {
    pub fn branch(&self, index: usize) -> Option<bool> {
        let i = &self.program.instructions[index];
        if i.is_conditional() {
            self.states[index]?.branch_condition(i)
        } else {
            None
        }
    }
    pub fn target(&self, index: usize) -> Option<u64> {
        let i = &self.program.instructions[index];
        if matches!(i.mnemonic(), "jmp" | "call") || i.is_conditional() {
            self.states[index]?.read(&i.operands[0])
        } else {
            None
        }
    }
    pub fn annotation(&self, index: usize) -> String {
        let Some(before) = self.states[index] else {
            return "no path from entry in this model".into();
        };
        let i = &self.program.instructions[index];
        let after = transfer(i, before);
        let mut notes = Vec::new();
        if !i.is_control_flow()
            && !matches!(i.op.as_str(), "cmp" | "test" | "push")
            && let Some(Operand::Reg(r, _)) = i.operands.first()
            && let Some(v) = after.registers[*r as usize]
        {
            notes.push(format!("{} = 0x{v:x}", asm_operand(&Operand::Reg(*r, 64))));
        }
        if i.is_counter_loop()
            && let Some(value) = after.registers[1]
        {
            notes.push(format!("rcx after decrement = 0x{value:x}"));
        }
        if let Some(taken) = self.branch(index) {
            let mut reasons = before
                .flags
                .entries()
                .into_iter()
                .filter_map(|(n, v)| v.map(|v| format!("{n}={}", u8::from(v))))
                .collect::<Vec<_>>();
            if (i.is_counter_loop() || i.op == "jrcxz")
                && let Some(count) = before.registers[1]
            {
                reasons.push(format!("RCX=0x{count:x}"));
            }
            notes.push(format!(
                "branch always {} ({})",
                if taken { "taken" } else { "falls through" },
                reasons.join(", ")
            ));
        }
        if matches!(i.mnemonic(), "jmp" | "call")
            && !matches!(i.operands[0], Operand::Imm(..))
            && let Some(target) = self.target(index)
        {
            notes.push(format!("resolved target = 0x{target:x}"));
        }
        let base_op = i.mnemonic();
        let memory_address = if matches!(base_op, "bt" | "bts" | "btr" | "btc") {
            crate::integer::bit_memory_address(i, &before)
        } else if base_op == "pop" {
            i.operands.iter().find_map(|o| after.address(o))
        } else {
            i.operands.iter().find_map(|o| before.address(o))
        };
        if let Some(address) = memory_address {
            notes.push(format!("effective address = 0x{address:x}"));
        }
        if i.op == "ret"
            && let Some(v) = before.registers[0]
        {
            notes.push(format!("rax at return = 0x{v:x}"));
        }
        if i.mnemonic() == "call" {
            notes.push("call barrier: return registers and flags unknown".into());
        }
        notes.join("; ")
    }
    pub fn text(&self) -> String {
        let mut s = format!(
            "Value analysis from 0x{:x}: {} instruction visits\nCalls forget registers/flags; memory values are unknown; returns follow normal call discipline.\n",
            self.program.entry, self.iterations
        );
        if self.conservative_indirect {
            s.push_str("Unknown indirect control flow: every decoded instruction receives unknown input.\n");
        }
        for (n, i) in self.program.instructions.iter().enumerate() {
            let note = self.annotation(n);
            writeln!(
                s,
                "0x{:x}: {}{}",
                i.address,
                listing(i),
                if note.is_empty() {
                    String::new()
                } else {
                    format!(" ; {note}")
                }
            )
            .unwrap();
        }
        s
    }
    pub fn json(&self) -> String {
        let mut s = format!(
            "{{\"schema\": \"rosemary-values-1\", \"iterations\": {}, \"conservative_indirect\": {}, \"instructions\": [",
            self.iterations, self.conservative_indirect
        );
        for (n, i) in self.program.instructions.iter().enumerate() {
            if n != 0 {
                s.push(',');
            }
            write!(s, "\n    {{\"address\": {}, \"reachable\": {}, \"branch_taken\": {}, \"target\": {}, \"registers_after\": {{",
                hex(i.address), self.states[n].is_some(), self.branch(n).map_or("null", |v| if v { "true" } else { "false" }), self.target(n).map_or("null".into(), hex)).unwrap();
            let after = self.states[n].map(|s| transfer(i, s));
            let mut first = true;
            if let Some(after) = after {
                for (r, value) in after.registers.iter().enumerate() {
                    if let Some(value) = value {
                        if !first {
                            s.push_str(", ");
                        }
                        first = false;
                        write!(
                            s,
                            "{}: {}",
                            quoted(&asm_operand(&Operand::Reg(r as u8, 64))),
                            hex(*value)
                        )
                        .unwrap();
                    }
                }
            }
            s.push_str("}, \"flags_after\": {");
            first = true;
            if let Some(after) = after {
                for (name, value) in after
                    .flags
                    .entries()
                    .into_iter()
                    .chain([("DF", after.direction)])
                {
                    if let Some(value) = value {
                        if !first {
                            s.push_str(", ");
                        }
                        first = false;
                        write!(s, "{}: {}", quoted(name), value).unwrap();
                    }
                }
            }
            write!(s, "}}, \"explanation\": {}}}", quoted(&self.annotation(n))).unwrap();
        }
        s.push_str("\n  ]}");
        s
    }

    /// Simplify only control transfers, retaining instruction addresses, lengths,
    /// flag effects and budget accounting. Counter loops retain their decrement.
    /// Other entry points are not assumed.
    pub fn simplify(&self) -> Result<Program> {
        let mut p = self.program.clone();
        for (n, i) in p.instructions.iter_mut().enumerate() {
            if i.is_counter_loop() {
                continue;
            }
            if let Some(taken) = self.branch(n) {
                i.op = if taken { "jmp" } else { "nop" }.into();
                if !taken {
                    i.operands.clear();
                }
            } else if matches!(i.mnemonic(), "jmp" | "call")
                && let Some(target) = self.target(n)
            {
                // A direct transfer no longer needs the NOTRACK prefix.
                i.op = i.mnemonic().into();
                i.operands = vec![Operand::Imm(target, 64)];
            }
        }
        p.validate()?;
        Ok(p)
    }
}
