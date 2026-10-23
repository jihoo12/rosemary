//! Deliberately bounded x86-64 decoder. Unknown encodings never guess a length.
use crate::{
    Result,
    ir::{CONDITIONS, Instruction, Operand, Program, Segment},
};
use std::collections::{BTreeMap, BTreeSet};
struct Reader<'a> {
    bytes: &'a [u8],
    pos: usize,
}
impl Reader<'_> {
    fn byte(&mut self) -> Result<u8> {
        let b = *self.bytes.get(self.pos).ok_or("truncated instruction")?;
        self.pos += 1;
        Ok(b)
    }
    fn uint(&mut self, n: usize) -> Result<u64> {
        let mut v = 0;
        for i in 0..n {
            v |= (self.byte()? as u64) << (i * 8);
        }
        Ok(v)
    }
    fn signed(&mut self, n: usize) -> Result<u64> {
        let v = self.uint(n)?;
        Ok(((v << (64 - n * 8)) as i64 >> (64 - n * 8)) as u64)
    }
    fn modrm(&mut self, rex: u8, w: u8) -> Result<(u8, Operand)> {
        let m = self.byte()?;
        let mode = m >> 6;
        let reg = ((m >> 3) & 7) | ((rex & 4) << 1);
        let rm = m & 7;
        if mode == 3 {
            return Ok((reg, register(rm | ((rex & 1) << 3), w, rex)?));
        }
        let mut base = Some(rm | ((rex & 1) << 3));
        let mut index = None;
        let mut scale = 1;
        let mut disp = 0;
        if rm == 4 {
            let sib = self.byte()?;
            scale = 1 << (sib >> 6);
            let ix = (sib >> 3) & 7;
            if ix != 4 || rex & 2 != 0 {
                index = Some(ix | ((rex & 2) << 2));
            }
            let b = sib & 7;
            base = Some(b | ((rex & 1) << 3));
            if mode == 0 && b == 5 {
                base = None;
                disp = self.signed(4)?;
            }
        } else if mode == 0 && rm == 5 {
            base = Some(16);
            disp = self.signed(4)?;
        }
        if mode == 1 {
            disp = self.signed(1)?;
        } else if mode == 2 {
            disp = self.signed(4)?;
        }
        Ok((
            reg,
            Operand::Mem {
                segment: None,
                width: w,
                base,
                index,
                scale,
                disp,
            },
        ))
    }
}
fn imm(v: u64, w: u8) -> Operand {
    Operand::Imm(if w == 64 { v } else { v & ((1u64 << w) - 1) }, w)
}
fn register(r: u8, w: u8, rex: u8) -> Result<Operand> {
    if w == 8 && rex == 0 && (4..8).contains(&r) {
        return Ok(Operand::High(r - 4));
    }
    Ok(Operand::Reg(r, w))
}

fn vector_modrm(r: &mut Reader<'_>, rex: u8, width: u8) -> Result<(u8, Operand)> {
    let (reg, rm) = r.modrm(rex, width)?;
    Ok((
        reg,
        match rm {
            Operand::Reg(r, _) => Operand::Vec(r),
            other => other,
        },
    ))
}
fn vector_instruction(
    r: &mut Reader<'_>,
    second: u8,
    rex: u8,
    prefix: u8,
) -> Result<Option<(String, Vec<Operand>)>> {
    if matches!(prefix, 0 | 0x66) && matches!(second, 0x12 | 0x13 | 0x16 | 0x17) {
        let (reg, rm) = vector_modrm(r, rex, 64)?;
        let store = second & 1 != 0;
        let high = second & 4 != 0;
        let op = if matches!(&rm, Operand::Vec(_)) {
            if prefix != 0 || store {
                return Err("invalid partial vector move encoding".into());
            }
            if high { "movlhps" } else { "movhlps" }
        } else {
            match (prefix, high) {
                (0, false) => "movlps",
                (0, true) => "movhps",
                (_, false) => "movlpd",
                (_, true) => "movhpd",
            }
        };
        return Ok(Some((
            op.into(),
            if store {
                vec![rm, Operand::Vec(reg)]
            } else {
                vec![Operand::Vec(reg), rm]
            },
        )));
    }
    let mapped = match (prefix, second) {
        (0, 0x10 | 0x11) => Some("movups"),
        (0, 0x28 | 0x29) => Some("movaps"),
        (0x66, 0x10 | 0x11) => Some("movupd"),
        (0x66, 0x28 | 0x29) => Some("movapd"),
        (0xf3, 0x6f | 0x7f) => Some("movdqu"),
        (0x66, 0x6f | 0x7f) => Some("movdqa"),
        (0xf2, 0x10 | 0x11) => Some("movsd"),
        (0xf3, 0x10 | 0x11) => Some("movss"),
        (0xf2, 0x12) => Some("movddup"),
        (0xf3, 0x12) => Some("movsldup"),
        (0xf3, 0x16) => Some("movshdup"),
        (0, 0x54) => Some("andps"),
        (0x66, 0x54) => Some("andpd"),
        (0, 0x55) => Some("andnps"),
        (0x66, 0x55) => Some("andnpd"),
        (0, 0x56) => Some("orps"),
        (0x66, 0x56) => Some("orpd"),
        (0, 0x57) => Some("xorps"),
        (0x66, 0x57) => Some("xorpd"),
        (0x66, 0xef) => Some("pxor"),
        (0x66, 0xdb) => Some("pand"),
        (0x66, 0xdf) => Some("pandn"),
        (0x66, 0xeb) => Some("por"),
        (0x66, 0xfc) => Some("paddb"),
        (0x66, 0xfd) => Some("paddw"),
        (0x66, 0xfe) => Some("paddd"),
        (0x66, 0xd4) => Some("paddq"),
        (0x66, 0xf8) => Some("psubb"),
        (0x66, 0xf9) => Some("psubw"),
        (0x66, 0xfa) => Some("psubd"),
        (0x66, 0xfb) => Some("psubq"),
        (0x66, 0xdc) => Some("paddusb"),
        (0x66, 0xdd) => Some("paddusw"),
        (0x66, 0xd8) => Some("psubusb"),
        (0x66, 0xd9) => Some("psubusw"),
        (0x66, 0xec) => Some("paddsb"),
        (0x66, 0xed) => Some("paddsw"),
        (0x66, 0xe8) => Some("psubsb"),
        (0x66, 0xe9) => Some("psubsw"),
        (0x66, 0xe0) => Some("pavgb"),
        (0x66, 0xe3) => Some("pavgw"),
        (0x66, 0x74) => Some("pcmpeqb"),
        (0x66, 0x75) => Some("pcmpeqw"),
        (0x66, 0x76) => Some("pcmpeqd"),
        (0x66, 0x64) => Some("pcmpgtb"),
        (0x66, 0x65) => Some("pcmpgtw"),
        (0x66, 0x66) => Some("pcmpgtd"),
        (0x66, 0xda) => Some("pminub"),
        (0x66, 0xde) => Some("pmaxub"),
        (0x66, 0xea) => Some("pminsw"),
        (0x66, 0xee) => Some("pmaxsw"),
        (0x66, 0x63) => Some("packsswb"),
        (0x66, 0x6b) => Some("packssdw"),
        (0x66, 0x67) => Some("packuswb"),
        (0x66, 0x60) => Some("punpcklbw"),
        (0x66, 0x61) => Some("punpcklwd"),
        (0x66, 0x62) => Some("punpckldq"),
        (0x66, 0x6c) => Some("punpcklqdq"),
        (0x66, 0x68) => Some("punpckhbw"),
        (0x66, 0x69) => Some("punpckhwd"),
        (0x66, 0x6a) => Some("punpckhdq"),
        (0x66, 0x6d) => Some("punpckhqdq"),
        (0x66, 0xd1) => Some("psrlw"),
        (0x66, 0xd2) => Some("psrld"),
        (0x66, 0xd3) => Some("psrlq"),
        (0x66, 0xe1) => Some("psraw"),
        (0x66, 0xe2) => Some("psrad"),
        (0x66, 0xf1) => Some("psllw"),
        (0x66, 0xf2) => Some("pslld"),
        (0x66, 0xf3) => Some("psllq"),
        (0x66, 0xf4) => Some("pmuludq"),
        (0x66, 0xd5) => Some("pmullw"),
        (0x66, 0xe5) => Some("pmulhw"),
        (0x66, 0xe4) => Some("pmulhuw"),
        (0x66, 0xf5) => Some("pmaddwd"),
        (0x66, 0xf6) => Some("psadbw"),
        _ => None,
    };
    if let Some(op) = mapped {
        let width = match op {
            "movss" => 32,
            "movsd" | "movddup" => 64,
            _ => 128,
        };
        let (reg, rm) = vector_modrm(r, rex, width)?;
        let operands = if matches!(second, 0x11 | 0x29 | 0x7f) {
            vec![rm, Operand::Vec(reg)]
        } else {
            vec![Operand::Vec(reg), rm]
        };
        return Ok(Some((op.into(), operands)));
    }
    if prefix == 0x66 && matches!(second, 0x6e | 0x7e) {
        let width = if rex & 8 != 0 { 64 } else { 32 };
        let (reg, rm) = r.modrm(rex, width)?;
        let operands = if second == 0x6e {
            vec![Operand::Vec(reg), rm]
        } else {
            vec![rm, Operand::Vec(reg)]
        };
        return Ok(Some((
            if width == 64 { "movq" } else { "movd" }.into(),
            operands,
        )));
    }
    if (prefix == 0xf3 && second == 0x7e) || (prefix == 0x66 && second == 0xd6) {
        let (reg, rm) = vector_modrm(r, rex, 64)?;
        return Ok(Some((
            "movq".into(),
            if second == 0x7e {
                vec![Operand::Vec(reg), rm]
            } else {
                vec![rm, Operand::Vec(reg)]
            },
        )));
    }
    if second == 0xc6 && matches!(prefix, 0 | 0x66) {
        let (reg, rm) = vector_modrm(r, rex, 128)?;
        return Ok(Some((
            if prefix == 0 { "shufps" } else { "shufpd" }.into(),
            vec![Operand::Vec(reg), rm, imm(r.uint(1)?, 8)],
        )));
    }
    if second == 0x70 && matches!(prefix, 0x66 | 0xf2 | 0xf3) {
        let (reg, rm) = vector_modrm(r, rex, 128)?;
        let select = imm(r.uint(1)?, 8);
        return Ok(Some((
            match prefix {
                0x66 => "pshufd",
                0xf2 => "pshuflw",
                _ => "pshufhw",
            }
            .into(),
            vec![Operand::Vec(reg), rm, select],
        )));
    }
    if prefix == 0x66 && (0x71..=0x73).contains(&second) {
        let (group, rm) = vector_modrm(r, rex, 128)?;
        let op = match (second, group & 7) {
            (0x71, 2) => "psrlw",
            (0x72, 2) => "psrld",
            (0x73, 2) => "psrlq",
            (0x71, 4) => "psraw",
            (0x72, 4) => "psrad",
            (0x71, 6) => "psllw",
            (0x72, 6) => "pslld",
            (0x73, 6) => "psllq",
            (0x73, 3) => "psrldq",
            (0x73, 7) => "pslldq",
            _ => return Err("unsupported packed shift extension".into()),
        };
        return Ok(Some((op.into(), vec![rm, imm(r.uint(1)?, 8)])));
    }
    if (prefix == 0x66 && second == 0xd7) || (matches!(prefix, 0 | 0x66) && second == 0x50) {
        let (reg, rm) = vector_modrm(r, rex, 128)?;
        return Ok(Some((
            if second == 0xd7 {
                "pmovmskb"
            } else if prefix == 0x66 {
                "movmskpd"
            } else {
                "movmskps"
            }
            .into(),
            vec![Operand::Reg(reg, 32), rm],
        )));
    }
    if prefix == 0x66 && second == 0xc4 {
        let (reg, rm) = r.modrm(rex, 16)?;
        let rm = match rm {
            Operand::Reg(r, _) => Operand::Reg(r, 32),
            other => other,
        };
        return Ok(Some((
            "pinsrw".into(),
            vec![Operand::Vec(reg), rm, imm(r.uint(1)?, 8)],
        )));
    }
    if prefix == 0x66 && second == 0xc5 {
        let (reg, rm) = vector_modrm(r, rex, 128)?;
        return Ok(Some((
            "pextrw".into(),
            vec![Operand::Reg(reg, 32), rm, imm(r.uint(1)?, 8)],
        )));
    }
    Ok(None)
}
fn one(r: &mut Reader<'_>, address: u64) -> Result<Instruction> {
    let start = r.pos;
    let mut opcode = r.byte()?;
    let mut operand16 = false;
    let mut cs_prefix = false;
    let mut repeat = 0;
    let mut segment = None;
    let mut locked = false;
    let mut notrack = false;
    while matches!(
        opcode,
        0x66 | 0x2e | 0x3e | 0xf2 | 0xf3 | 0x64 | 0x65 | 0xf0
    ) {
        notrack |= opcode == 0x3e;
        if opcode == 0xf0 {
            if locked {
                return Err("duplicate LOCK prefix".into());
            }
            locked = true;
        }
        if matches!(opcode, 0x64 | 0x65) {
            if segment.is_some() {
                return Err("multiple segment prefixes unsupported".into());
            }
            segment = Some(if opcode == 0x64 {
                Segment::Fs
            } else {
                Segment::Gs
            });
        }
        operand16 |= opcode == 0x66;
        cs_prefix |= opcode == 0x2e;
        if matches!(opcode, 0xf2 | 0xf3) {
            if repeat != 0 {
                return Err("multiple repeat prefixes unsupported".into());
            }
            repeat = opcode;
        }
        if r.pos - start >= 15 {
            return Err("instruction exceeds 15 bytes".into());
        }
        opcode = r.byte()?;
    }
    let mut rex = 0;
    if (0x40..=0x4f).contains(&opcode) {
        rex = opcode;
        opcode = r.byte()?;
    }
    let w = if rex & 8 != 0 {
        64
    } else if operand16 {
        16
    } else {
        32
    };
    if cs_prefix && !matches!(opcode, 0x90 | 0x0f) && !(notrack && opcode == 0xff) {
        return Err("CS prefix supported only for alignment NOPs".into());
    }
    if operand16
        && matches!(opcode, 0x50..=0x5f | 0x68 | 0x6a | 0x8f | 0xc2 | 0xc3 | 0xc9 | 0xe8 | 0xe9 | 0xeb | 0x70..=0x7f)
    {
        return Err("16-bit stack/control forms unsupported".into());
    }
    if repeat != 0
        && operand16
        && !matches!(opcode, 0xa4 | 0xa5 | 0xaa | 0xab)
        && !(repeat == 0xf3 && opcode == 0x0f)
    {
        return Err("multiple mandatory prefixes unsupported".into());
    }
    if repeat != 0
        && opcode != 0x0f
        && !(repeat == 0xf3 && matches!(opcode, 0x90 | 0xa4 | 0xa5 | 0xaa | 0xab))
    {
        return Err("unsupported repeat-prefixed instruction".into());
    }
    let mut a = Vec::new();
    let op: String;
    match opcode {
        0xf8 => op = "clc".into(),
        0xf9 => op = "stc".into(),
        0xf5 => op = "cmc".into(),
        0xfc => op = "cld".into(),
        0xfd => op = "std".into(),
        0xa4 | 0xa5 | 0xaa | 0xab => {
            if segment.is_some() {
                return Err("segmented string sources unsupported".into());
            }
            let width = if opcode & 1 == 0 { 8 } else { w };
            let base = if opcode < 0xaa { "movs" } else { "stos" };
            op = if repeat == 0xf3 {
                format!("rep.{base}")
            } else {
                base.into()
            };
            a = vec![imm(width as u64, 8)];
        }
        // F3 selects PAUSE before REX.B can select the extended XCHG form.
        0x90 if repeat == 0xf3 || rex & 1 == 0 => {
            op = if repeat == 0xf3 { "pause" } else { "nop" }.into()
        }
        0x90..=0x97 => {
            op = "xchg".into();
            a = vec![
                Operand::Reg(0, w),
                Operand::Reg((opcode & 7) | ((rex & 1) << 3), w),
            ];
        }
        0x98 => {
            op = if w == 64 { "movsxd" } else { "movsx" }.into();
            a = vec![Operand::Reg(0, w), Operand::Reg(0, w / 2)];
        }
        0x99 => {
            op = match w {
                16 => "cwd",
                32 => "cdq",
                _ => "cqo",
            }
            .into()
        }
        0x86 | 0x87 => {
            let w = if opcode == 0x86 { 8 } else { w };
            let (reg, rm) = r.modrm(rex, w)?;
            op = "xchg".into();
            a = vec![rm, register(reg, w, rex)?];
        }
        0xcc => op = "int3".into(),
        0xf4 => op = "hlt".into(),
        0xc2 => {
            op = "ret".into();
            a.push(imm(r.uint(2)?, 16));
        }
        0xc3 => op = "ret".into(),
        0xc9 => op = "leave".into(),
        0x50..=0x57 => {
            op = "push".into();
            a.push(Operand::Reg((opcode - 0x50) | ((rex & 1) << 3), 64));
        }
        0x58..=0x5f => {
            op = "pop".into();
            a.push(Operand::Reg((opcode - 0x58) | ((rex & 1) << 3), 64));
        }
        0x68 | 0x6a => {
            op = "push".into();
            a.push(imm(r.signed(if opcode == 0x68 { 4 } else { 1 })?, 64));
        }
        0xb0..=0xbf => {
            let w = if opcode < 0xb8 { 8 } else { w };
            op = "mov".into();
            a.push(register((opcode & 7) | ((rex & 1) << 3), w, rex)?);
            a.push(imm(r.uint((w / 8) as usize)?, w));
        }
        0x63 if w == 64 => {
            let (reg, rm) = r.modrm(rex, 32)?;
            op = "movsxd".into();
            a = vec![Operand::Reg(reg, 64), rm];
        }
        0x69 | 0x6b => {
            let (reg, rm) = r.modrm(rex, w)?;
            op = "imul".into();
            a = vec![
                Operand::Reg(reg, w),
                rm,
                imm(
                    r.signed(if opcode == 0x6b {
                        1
                    } else if w == 16 {
                        2
                    } else {
                        4
                    })?,
                    w,
                ),
            ];
        }
        0xf6 | 0xf7 => {
            let w = if opcode == 0xf6 { 8 } else { w };
            let (group, rm) = r.modrm(rex, w)?;
            op = match group & 7 {
                0 => "test",
                2 => "not",
                3 => "neg",
                4 => "mul",
                5 => "imul",
                6 => "div",
                7 => "idiv",
                _ => return Err("unsupported F7 extension".into()),
            }
            .into();
            a.push(rm);
            if group & 7 == 0 {
                a.push(imm(r.signed(if w < 32 { (w / 8) as usize } else { 4 })?, w));
            }
        }
        0x10..=0x13
        | 0x18..=0x1b
        | 0x88
        | 0x8a
        | 0x00
        | 0x02
        | 0x28
        | 0x2a
        | 0x30
        | 0x32
        | 0x20
        | 0x22
        | 0x08
        | 0x0a
        | 0x38
        | 0x3a
        | 0x84
        | 0x89
        | 0x8b
        | 0x8d
        | 0x01
        | 0x03
        | 0x29
        | 0x2b
        | 0x31
        | 0x33
        | 0x21
        | 0x23
        | 0x09
        | 0x0b
        | 0x39
        | 0x3b
        | 0x85 => {
            let w = if opcode & 1 == 0 { 8 } else { w };
            let (reg, rm) = r.modrm(rex, w)?;
            let reg = register(reg, w, rex)?;
            op = match opcode {
                0x88..=0x8b => "mov",
                0x8d => "lea",
                0x00..=0x03 => "add",
                0x10..=0x13 => "adc",
                0x18..=0x1b => "sbb",
                0x28..=0x2b => "sub",
                0x30..=0x33 => "xor",
                0x20..=0x23 => "and",
                0x08..=0x0b => "or",
                0x38..=0x3b => "cmp",
                _ => "test",
            }
            .into();
            if matches!(
                opcode,
                0x12 | 0x13
                    | 0x1a
                    | 0x1b
                    | 0x8a
                    | 0x8b
                    | 0x8d
                    | 0x02
                    | 0x03
                    | 0x2a
                    | 0x2b
                    | 0x32
                    | 0x33
                    | 0x22
                    | 0x23
                    | 0x0a
                    | 0x0b
                    | 0x3a
                    | 0x3b
            ) {
                a = vec![reg, rm];
            } else {
                a = vec![rm, reg];
            }
        }
        0xc6 | 0xc7 | 0x80 | 0x81 | 0x83 => {
            let w = if matches!(opcode, 0xc6 | 0x80) { 8 } else { w };
            let (group, rm) = r.modrm(rex, w)?;
            // REX.R does not extend opcode extension fields.
            let group = group & 7;
            op = if matches!(opcode, 0xc6 | 0xc7) {
                if group != 0 {
                    return Err("unsupported C7 extension".into());
                }
                "mov"
            } else {
                match group {
                    0 => "add",
                    1 => "or",
                    2 => "adc",
                    3 => "sbb",
                    4 => "and",
                    5 => "sub",
                    6 => "xor",
                    7 => "cmp",
                    _ => return Err("unsupported immediate arithmetic extension".into()),
                }
            }
            .into();
            a = vec![
                rm,
                imm(
                    r.signed(if opcode == 0x83 || w == 8 {
                        1
                    } else if w == 16 {
                        2
                    } else {
                        4
                    })?,
                    w,
                ),
            ];
        }
        0x14 | 0x15 | 0x1c | 0x1d | 0x04 | 0x0c | 0x24 | 0x2c | 0x34 | 0x3c | 0xa8 | 0x05
        | 0x0d | 0x25 | 0x2d | 0x35 | 0x3d | 0xa9 => {
            let w = if opcode & 1 == 0 { 8 } else { w };
            op = match opcode | 1 {
                0x05 => "add",
                0x0d => "or",
                0x15 => "adc",
                0x1d => "sbb",
                0x25 => "and",
                0x2d => "sub",
                0x35 => "xor",
                0x3d => "cmp",
                _ => "test",
            }
            .into();
            a = vec![
                Operand::Reg(0, w),
                imm(r.signed(if w < 32 { (w / 8) as usize } else { 4 })?, w),
            ];
        }
        0xc0 | 0xc1 | 0xd0..=0xd3 => {
            let w = if opcode & 1 == 0 { 8 } else { w };
            let (g, rm) = r.modrm(rex, w)?;
            op = match g & 7 {
                0 => "rol",
                1 => "ror",
                2 => "rcl",
                3 => "rcr",
                4 | 6 => "shl",
                5 => "shr",
                7 => "sar",
                _ => return Err("unsupported rotate/shift extension".into()),
            }
            .into();
            let count = match opcode {
                0xc0 | 0xc1 => imm(r.uint(1)?, 8),
                0xd0 | 0xd1 => imm(1, 8),
                _ => Operand::Reg(1, 8),
            };
            a = vec![rm, count];
        }
        0xe0..=0xe3 | 0xe8 | 0xe9 | 0xeb | 0x70..=0x7f => {
            op = match opcode {
                0xe0 => "loopne",
                0xe1 => "loope",
                0xe2 => "loop",
                0xe3 => "jrcxz",
                0xe8 => "call",
                0xe9 | 0xeb => "jmp",
                _ => CONDITIONS[(opcode & 15) as usize],
            }
            .into();
            let d = r.signed(if matches!(opcode, 0xe8 | 0xe9) { 4 } else { 1 })?;
            let next = address
                .checked_add((r.pos - start) as u64)
                .ok_or("address overflow")?;
            a.push(imm(next.wrapping_add(d), 64));
        }
        0x0f => {
            let second = r.byte()?;
            // F3 selects the count operation; 66 independently selects its
            // 16-bit operand size. Other mandatory-prefix combinations remain
            // unsupported rather than silently changing SSE instruction kinds.
            if repeat != 0 && operand16 && !matches!(second, 0xb8 | 0xbc | 0xbd) {
                return Err("multiple mandatory prefixes unsupported".into());
            }
            if cs_prefix && second != 0x1f {
                return Err("CS prefix supported only for alignment NOPs".into());
            }
            let prefix = if repeat != 0 {
                repeat
            } else if operand16 {
                0x66
            } else {
                0
            };
            if repeat == 0 && matches!(second, 0x2e | 0x2f) {
                let width = if operand16 { 64 } else { 32 };
                let (reg, rm) = vector_modrm(r, rex, width)?;
                op = match (second, operand16) {
                    (0x2e, false) => "ucomiss",
                    (0x2e, true) => "ucomisd",
                    (_, false) => "comiss",
                    (_, true) => "comisd",
                }
                .into();
                a = vec![Operand::Vec(reg), rm];
            } else if repeat == 0xf3 && second == 0x1e {
                if r.byte()? != 0xfa {
                    return Err("unsupported ENDBR encoding".into());
                }
                op = "endbr64".into();
            } else if repeat == 0 && second == 0x0b {
                op = "ud2".into();
            } else if repeat == 0 && second == 0x38 {
                let third = r.byte()?;
                if matches!(third, 0xf0 | 0xf1) {
                    let (reg, rm) = r.modrm(rex, w)?;
                    if !matches!(rm, Operand::Mem { .. }) {
                        return Err("MOVBE requires a memory operand".into());
                    }
                    op = "movbe".into();
                    a = if third == 0xf0 {
                        vec![Operand::Reg(reg, w), rm]
                    } else {
                        vec![rm, Operand::Reg(reg, w)]
                    };
                } else if operand16 && matches!(third, 0x00 | 0x04) {
                    let (reg, rm) = vector_modrm(r, rex, 128)?;
                    op = if third == 0x00 { "pshufb" } else { "pmaddubsw" }.into();
                    a = vec![Operand::Vec(reg), rm];
                } else {
                    return Err(format!("unsupported opcode 0f 38 {third:02x}"));
                }
            } else if repeat == 0 && second == 0x3a {
                let third = r.byte()?;
                if operand16 && third == 0x0f {
                    let (reg, rm) = vector_modrm(r, rex, 128)?;
                    let count = r.byte()?;
                    op = "palignr".into();
                    a = vec![Operand::Vec(reg), rm, imm(count.into(), 8)];
                } else {
                    return Err(format!("unsupported opcode 0f 3a {third:02x}"));
                }
            } else if let Some((name, operands)) = vector_instruction(r, second, rex, prefix)? {
                op = name;
                a = operands;
            } else if (matches!(second, 0xbc | 0xbd) && matches!(repeat, 0 | 0xf3))
                || (second == 0xb8 && repeat == 0xf3)
            {
                let (reg, rm) = r.modrm(rex, w)?;
                op = match (repeat, second) {
                    (0, 0xbc) => "bsf",
                    (0, _) => "bsr",
                    (_, 0xb8) => "popcnt",
                    (_, 0xbc) => "tzcnt",
                    _ => "lzcnt",
                }
                .into();
                a = vec![Operand::Reg(reg, w), rm];
            } else if repeat != 0 {
                return Err(format!(
                    "unsupported repeat-prefixed opcode 0f {second:02x}"
                ));
            } else if (0x80..=0x8f).contains(&second) {
                if operand16 {
                    return Err("16-bit control forms unsupported".into());
                }
                op = CONDITIONS[(second & 15) as usize].into();
                let d = r.signed(4)?;
                let next = address
                    .checked_add((r.pos - start) as u64)
                    .ok_or("address overflow")?;
                a.push(imm(next.wrapping_add(d), 64));
            } else if matches!(second, 0xb0 | 0xb1 | 0xc0 | 0xc1) {
                let w = if second & 1 == 0 { 8 } else { w };
                let (reg, rm) = r.modrm(rex, w)?;
                op = if second < 0xc0 { "cmpxchg" } else { "xadd" }.into();
                a = vec![rm, register(reg, w, rex)?];
            } else if matches!(second, 0xa4 | 0xa5 | 0xac | 0xad) {
                let (reg, rm) = r.modrm(rex, w)?;
                let count = if second & 1 == 0 {
                    imm(r.uint(1)?, 8)
                } else {
                    Operand::Reg(1, 8)
                };
                op = if second < 0xac { "shld" } else { "shrd" }.into();
                a = vec![rm, Operand::Reg(reg, w), count];
            } else if matches!(second, 0xa3 | 0xab | 0xb3 | 0xbb) {
                let (reg, rm) = r.modrm(rex, w)?;
                op = match second {
                    0xa3 => "bt",
                    0xab => "bts",
                    0xb3 => "btr",
                    _ => "btc",
                }
                .into();
                a = vec![rm, Operand::Reg(reg, w)];
            } else if second == 0xba {
                let (g, rm) = r.modrm(rex, w)?;
                op = match g & 7 {
                    4 => "bt",
                    5 => "bts",
                    6 => "btr",
                    7 => "btc",
                    _ => return Err("unsupported bit extension".into()),
                }
                .into();
                a = vec![rm, imm(r.uint(1)?, 8)];
            } else if (0xc8..=0xcf).contains(&second) {
                op = "bswap".into();
                a = vec![Operand::Reg((second & 7) | ((rex & 1) << 3), w)];
            } else if matches!(second, 0xb6 | 0xb7 | 0xbe | 0xbf) {
                let (reg, rm) = r.modrm(rex, if second & 1 == 0 { 8 } else { 16 })?;
                op = if second < 0xbe { "movzx" } else { "movsx" }.into();
                a = vec![Operand::Reg(reg, w), rm];
            } else if (0x40..=0x4f).contains(&second) {
                let (reg, rm) = r.modrm(rex, w)?;
                op = format!("cmov{}", &CONDITIONS[(second & 15) as usize][1..]);
                a = vec![Operand::Reg(reg, w), rm];
            } else if (0x90..=0x9f).contains(&second) {
                let (_, rm) = r.modrm(rex, 8)?;
                op = format!("set{}", &CONDITIONS[(second & 15) as usize][1..]);
                a = vec![rm];
            } else if second == 0xaf {
                let (reg, rm) = r.modrm(rex, w)?;
                op = "imul".into();
                a = vec![Operand::Reg(reg, w), rm];
            } else if second == 0x1f {
                let (g, m) = r.modrm(rex, w)?;
                if g & 7 != 0 || matches!(m, Operand::Reg(..)) {
                    return Err("invalid NOP encoding".into());
                }
                op = "nop".into();
            } else {
                return Err(format!("unsupported opcode 0f {second:02x}"));
            }
        }
        0xfe | 0xff => {
            // INC/DEC use operand size; calls, jumps and pushes use 64 bits.
            let group = (r.bytes.get(r.pos).ok_or("truncated instruction")? >> 3) & 7;
            if (operand16 && group > 1) || (opcode == 0xfe && group > 1) {
                return Err("unsupported stack/control extension".into());
            }
            let (g, m) = r.modrm(
                rex,
                if opcode == 0xfe {
                    8
                } else if group <= 1 {
                    w
                } else {
                    64
                },
            )?;
            op = match g & 7 {
                0 => "inc",
                1 => "dec",
                2 => "call",
                4 => "jmp",
                6 => "push",
                _ => return Err("unsupported FF extension".into()),
            }
            .into();
            a.push(m);
        }
        0x8f => {
            let (g, m) = r.modrm(rex, 64)?;
            if g & 7 != 0 {
                return Err("invalid POP extension".into());
            }
            op = "pop".into();
            a.push(m);
        }
        _ => return Err(format!("unsupported opcode {opcode:02x}")),
    }
    if notrack
        && (locked
            || !matches!(op.as_str(), "call" | "jmp")
            || !matches!(
                a.as_slice(),
                [Operand::Reg(_, 64) | Operand::Mem { width: 64, .. }]
            ))
    {
        return Err("3E prefix supported only on indirect calls and jumps without LOCK".into());
    }
    // In 64-bit mode FS/GS suppress recognition of 3E as NOTRACK, regardless
    // of prefix order. Retain only the effective segment on those transfers.
    let op = if notrack && segment.is_none() {
        format!("notrack.{op}")
    } else {
        op
    };
    let op = if locked { format!("lock.{op}") } else { op };
    let len = r.pos - start;
    if len > 15 {
        return Err("instruction exceeds 15 bytes".into());
    }
    let next = address.checked_add(len as u64).ok_or("address overflow")?;
    for o in &mut a {
        if let Operand::Mem { base, disp, .. } = o
            && *base == Some(16)
        {
            *base = None;
            *disp = disp.wrapping_add(next);
        }
        if let Operand::Mem { segment: s, .. } = o
            && op != "lea"
        {
            *s = segment;
        }
    }
    Ok(Instruction {
        address,
        len: len as u8,
        op,
        operands: a,
    })
}
pub fn decode(bytes: &[u8], base: u64, entry: Option<u64>) -> Result<Program> {
    decode_linear(bytes, base, entry, usize::MAX)
}
/// Decode every executable PROGBITS section without guessing through gaps.
pub fn decode_elf(bytes: &[u8], entry: Option<u64>, max_instructions: usize) -> Result<Program> {
    let sections = crate::elf::elf_executable_code(bytes)?;
    let mut instructions = Vec::new();
    for section in sections {
        let p = decode_linear(
            section.bytes,
            section.base,
            Some(section.base),
            max_instructions.saturating_sub(instructions.len()),
        )?;
        instructions.extend(p.instructions);
    }
    instructions.sort_by_key(|i| i.address);
    let default = crate::elf::elf_entry(bytes)?;
    let p = Program {
        entry: entry.unwrap_or(if default == 0 {
            instructions
                .first()
                .ok_or("ELF has no executable code")?
                .address
        } else {
            default
        }),
        instructions,
    };
    p.validate()?;
    Ok(p)
}
pub fn decode_linear(
    bytes: &[u8],
    base: u64,
    entry: Option<u64>,
    max_instructions: usize,
) -> Result<Program> {
    let mut r = Reader { bytes, pos: 0 };
    let mut instructions = Vec::new();
    while r.pos < bytes.len() {
        let address = base.checked_add(r.pos as u64).ok_or("address overflow")?;
        if instructions.len() >= max_instructions {
            return Err(format!(
                "0x{address:x}: linear instruction limit ({max_instructions}) exceeded"
            ));
        }
        instructions.push(one(&mut r, address).map_err(|e| format!("0x{address:x}: {e}"))?);
    }
    let p = Program {
        entry: entry.unwrap_or(base),
        instructions,
    };
    p.validate()?;
    Ok(p)
}

#[derive(Clone, Copy, Debug)]
pub struct RecursiveOptions {
    /// Follow direct calls within the input, as well as their continuations.
    pub follow_calls: bool,
    /// Bound work even for untrusted, very large inputs.
    pub max_instructions: usize,
}

impl Default for RecursiveOptions {
    fn default() -> Self {
        Self {
            follow_calls: true,
            max_instructions: 1_000_000,
        }
    }
}

/// Recover reachable code without interpreting skipped bytes as instructions.
/// Indirect transfers are retained in the IR, but their targets are not guessed.
pub fn decode_recursive(
    bytes: &[u8],
    base: u64,
    entry: u64,
    options: RecursiveOptions,
) -> Result<Program> {
    let end = base
        .checked_add(bytes.len() as u64)
        .ok_or("address overflow")?;
    if !(base..end).contains(&entry) {
        return Err("entry outside input code".into());
    }
    let mut pending = BTreeSet::from([entry]);
    let mut decoded: BTreeMap<u64, Instruction> = BTreeMap::new();
    while let Some(address) = pending.pop_first() {
        if !(base..end).contains(&address) || decoded.contains_key(&address) {
            continue;
        }
        if let Some((_, previous)) = decoded.range(..address).next_back()
            && previous.address + previous.len as u64 > address
        {
            return Err(format!(
                "0x{address:x}: control flow enters instruction at 0x{:x}",
                previous.address
            ));
        }
        if decoded.len() >= options.max_instructions {
            return Err(format!(
                "0x{address:x}: recursive instruction limit ({}) exceeded",
                options.max_instructions
            ));
        }
        let mut reader = Reader {
            bytes,
            pos: (address - base) as usize,
        };
        let instruction = one(&mut reader, address).map_err(|e| format!("0x{address:x}: {e}"))?;
        let next = address + instruction.len as u64;
        if let Some((&other, _)) = decoded.range(address..next).next() {
            return Err(format!(
                "0x{address:x}: instruction overlaps decoded instruction at 0x{other:x}"
            ));
        }
        if instruction.falls_through() {
            pending.insert(next);
        }
        if (instruction.mnemonic() != "call" || options.follow_calls)
            && let Some(target) = instruction.direct_target()
        {
            pending.insert(target);
        }
        decoded.insert(address, instruction);
    }
    let p = Program {
        entry,
        instructions: decoded.into_values().collect(),
    };
    p.validate()?;
    Ok(p)
}
