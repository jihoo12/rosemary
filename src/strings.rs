//! String instructions advance one element per interpreter budget unit.
use crate::{
    dataflow::State,
    ir::{Instruction, Operand},
};
use std::fmt::Write;
pub(crate) fn valid(i: &Instruction) -> Option<bool> {
    Some(match i.op.as_str() {
        "cld" | "std" => i.operands.is_empty(),
        "movs" | "stos" | "rep.movs" | "rep.stos" => {
            matches!(i.operands.as_slice(), [Operand::Imm(8 | 16 | 32 | 64, 8)])
        }
        _ => return None,
    })
}
pub(crate) fn assembly(i: &Instruction) -> String {
    if matches!(i.op.as_str(), "cld" | "std") {
        return i.op.clone();
    }
    let [Operand::Imm(w, _)] = i.operands.as_slice() else {
        unreachable!()
    };
    let base = i.op.strip_prefix("rep.").unwrap_or(&i.op);
    format!(
        "{}{base}{}",
        if i.op.starts_with("rep.") { "rep " } else { "" },
        match w {
            8 => "b",
            16 => "w",
            32 => "d",
            _ => "q",
        }
    )
}
pub(crate) fn emit(i: &Instruction) -> String {
    if matches!(i.op.as_str(), "cld" | "std") {
        return format!("m->df={};", u8::from(i.op == "std"));
    }
    let [Operand::Imm(w, _)] = i.operands.as_slice() else {
        unreachable!()
    };
    let repeated = i.op.starts_with("rep.");
    let moving = i.op.ends_with("movs");
    let bytes = w / 8;
    let mut s = String::new();
    if repeated {
        s.push_str("if(m->r[1]){\n");
    }
    writeln!(s, "uint64_t step=m->df?UINT64_C(0)-{bytes}:{bytes};").unwrap();
    if moving {
        writeln!(s,"uint64_t value=m->load(m->ctx,m->r[6],{bytes});m->store(m->ctx,m->r[7],{bytes},value);m->r[6]+=step;").unwrap();
    } else {
        writeln!(s, "m->store(m->ctx,m->r[7],{bytes},m->r[0]);").unwrap();
    }
    s.push_str("m->r[7]+=step;\n");
    if repeated {
        writeln!(
            s,
            "m->r[1]--;if(m->r[1]){{m->rip=UINT64_C(0x{:x});continue;}}}}",
            i.address
        )
        .unwrap();
    }
    s
}
pub(crate) fn transfer(i: &Instruction, mut s: State) -> State {
    if matches!(i.op.as_str(), "cld" | "std") {
        s.direction = Some(i.op == "std");
        return s;
    }
    let [Operand::Imm(w, _)] = i.operands.as_slice() else {
        unreachable!()
    };
    let repeated = i.op.starts_with("rep.");
    if repeated && s.registers[1] == Some(0) {
        return s;
    }
    if repeated {
        s.registers[1] = s.registers[1].map(|v| v.wrapping_sub(1));
    }
    let step = s.direction.map(|backwards| {
        if backwards {
            0u64.wrapping_sub(w / 8)
        } else {
            w / 8
        }
    });
    for reg in if i.op.ends_with("movs") {
        &[6, 7][..]
    } else {
        &[7][..]
    } {
        s.registers[*reg] = s.registers[*reg].zip(step).map(|(v, d)| v.wrapping_add(d));
    }
    s
}
