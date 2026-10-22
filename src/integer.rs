//! Extended integer semantics needed by optimized Rust compiler output.
use crate::{
    dataflow::{Flags, State, result_flags},
    emit::{address, put, read},
    ir::{Instruction, Operand},
};
use std::fmt::Write;
pub(crate) fn valid(i: &Instruction) -> Option<bool> {
    let op = i.op.as_str();
    let a = &i.operands;
    let writable = |o: &Operand| {
        matches!(
            o,
            Operand::High(_)
                | Operand::Reg(_, 8 | 16 | 32 | 64)
                | Operand::Mem {
                    width: 8 | 16 | 32 | 64,
                    ..
                }
        )
    };
    Some(match op {
        "xadd" | "cmpxchg" => {
            matches!(a.as_slice(),[dst,src] if writable(dst)&&matches!(src,Operand::Reg(..)|Operand::High(_))&&dst.width()==src.width())
        }
        "rol" | "ror" | "rcl" | "rcr" => {
            a.len() == 2
                && writable(&a[0])
                && matches!(a[1], Operand::Imm(_, 8) | Operand::Reg(1, 8))
        }
        "shld" | "shrd" => {
            matches!(a.as_slice(),[dst,Operand::Reg(_,w),count] if writable(dst)&&*w>=16&&dst.width()==*w&&matches!(count,Operand::Imm(_,8)|Operand::Reg(1,8)))
        }
        "adc" | "sbb" | "xchg" => {
            a.len() == 2
                && writable(&a[0])
                && a[0].width() == a[1].width()
                && (op != "xchg" || writable(&a[1]))
                && !(matches!(a[0], Operand::Mem { .. }) && matches!(a[1], Operand::Mem { .. }))
        }
        "mul" | "div" | "idiv" => a.len() == 1 && writable(&a[0]),
        "imul" if a.len() == 1 => writable(&a[0]),
        "bt" | "bts" | "btr" | "btc" => {
            a.len() == 2
                && writable(&a[0])
                && a[0].width() >= 16
                && matches!(a[1],Operand::Reg(_,w) if w==a[0].width())
                || matches!(op, "bt" | "bts" | "btr" | "btc")
                    && a.len() == 2
                    && writable(&a[0])
                    && a[0].width() >= 16
                    && matches!(a[1], Operand::Imm(_, 8))
        }
        "bsf" | "bsr" | "tzcnt" | "lzcnt" | "popcnt" => {
            matches!(a.as_slice(),[Operand::Reg(_,w),src] if *w>=16&&writable(src)&&src.width()==*w)
        }
        "bswap" => matches!(a.as_slice(), [Operand::Reg(_, 32 | 64)]),
        "cwd" | "cdq" | "cqo" | "clc" | "stc" | "cmc" => a.is_empty(),
        _ => return None,
    })
}
pub(crate) const HELPERS: &str = r#"
static inline void rosemary_mulwide(uint64_t a,uint64_t b,uint64_t *lo,uint64_t *hi) {
    uint64_t a0=(uint32_t)a,a1=a>>32,b0=(uint32_t)b,b1=b>>32;
    uint64_t w0=a0*b0,t=a1*b0+(w0>>32),w1=(uint32_t)t,w2=t>>32;
    w1+=a0*b1; *hi=a1*b1+w2+(w1>>32); *lo=(w1<<32)|(uint32_t)w0;
}
static inline unsigned rosemary_divwide(uint64_t hi,uint64_t lo,uint64_t den,unsigned w,unsigned is_signed,uint64_t *q,uint64_t *r) {
    uint64_t mask=rosemary_vmask(w),sign=UINT64_C(1)<<(w-1);
    unsigned negative=is_signed&&(hi&sign)!=0, denominator_negative=is_signed&&(den&sign)!=0;
    if(negative) {lo=(~lo+1)&mask;hi=(~hi+(lo==0))&mask;}
    if(denominator_negative) den=(~den+1)&mask;
    if(!den||hi>=den) return 0;
    uint64_t quotient=0,remainder=hi;
    for(unsigned j=w;j>0;j--){
        unsigned carry=(remainder&sign)!=0;
        remainder=((remainder<<1)|((lo>>(j-1))&1))&mask;
        if(carry||remainder>=den){remainder=(remainder-den)&mask;quotient|=UINT64_C(1)<<(j-1);}
    }
    unsigned quotient_negative=negative!=denominator_negative;
    if(is_signed && quotient>(quotient_negative?sign:sign-1)) return 0;
    *q=quotient_negative?(~quotient+1)&mask:quotient;
    *r=negative?(~remainder+1)&mask:remainder;return 1;
}
"#;
fn ea(s: &mut String, o: &Operand) {
    if matches!(o, Operand::Mem { .. }) {
        writeln!(s, "uint64_t ea = {};", address(o)).unwrap();
    }
}
fn mask(w: u8) -> u64 {
    if w == 64 { u64::MAX } else { (1u64 << w) - 1 }
}
fn accumulator(w: u8) -> Operand {
    Operand::Reg(0, if w == 8 { 16 } else { w })
}
fn high_register(w: u8) -> Operand {
    Operand::Reg(2, w)
}
fn bit_address(o: &Operand, index: &Operand) -> String {
    let Operand::Mem { width, .. } = o else {
        unreachable!()
    };
    let mut a = address(o);
    if matches!(index, Operand::Reg(..)) {
        let log = width.trailing_zeros();
        let sign = 1u64 << (*width - 1);
        // Sign-extend the bit offset before floor-dividing by the operand width.
        let signed = format!(
            "(({} ^ UINT64_C(0x{sign:x})) - UINT64_C(0x{sign:x}))",
            read(index)
        );
        a = format!(
            "({a} + ((({signed} >> {log}) | (({signed} >> 63) ? (UINT64_MAX << {}) : 0)) * {}))",
            64 - log,
            width / 8
        );
    }
    a
}
pub(crate) fn bit_memory_address(i: &Instruction, s: &State) -> Option<u64> {
    let a = &i.operands;
    let base = s.address(a.first()?)?;
    if let Some(index @ Operand::Reg(_, w)) = a.get(1) {
        let raw = s.read(index)?;
        let signed = (raw << (64 - w)) as i64 >> (64 - w);
        let offset = (signed >> w.trailing_zeros()) as u64;
        Some(base.wrapping_add(offset.wrapping_mul((*w / 8) as u64)))
    } else {
        Some(base)
    }
}
pub(crate) fn emit(i: &Instruction) -> String {
    let op = i.op.as_str();
    let a = &i.operands;
    let mut s = String::new();
    if matches!(op, "clc" | "stc" | "cmc") {
        return match op {
            "clc" => "m->cf=0;",
            "stc" => "m->cf=1;",
            _ => "m->cf=!m->cf;",
        }
        .into();
    }
    if matches!(op, "cwd" | "cdq" | "cqo") {
        let w = match op {
            "cwd" => 16,
            "cdq" => 32,
            _ => 64,
        };
        return put(
            &Operand::Reg(2, w),
            &format!("(m->r[0] & (UINT64_C(1)<<{})) ? UINT64_MAX : 0", w - 1),
        );
    }
    let w = a[0].width();
    if matches!(op, "xadd" | "cmpxchg") {
        ea(&mut s, &a[0]);
        writeln!(
            s,
            "uint64_t destination={},source={};",
            read(&a[0]),
            read(&a[1])
        )
        .unwrap();
        if op == "xadd" {
            writeln!(s,"uint64_t v=(destination+source)&UINT64_C(0x{:x});rosemary_flags(m,destination,source,v,{w},1);",mask(w)).unwrap();
            s.push_str(&put(&a[1], "destination"));
            s.push_str(&put(&a[0], "v"));
        } else {
            writeln!(s, "uint64_t accumulator={};", read(&Operand::Reg(0, w))).unwrap();
            writeln!(s,"uint64_t v=(accumulator-destination)&UINT64_C(0x{:x});rosemary_flags(m,accumulator,destination,v,{w},2);if(m->zf){{",mask(w)).unwrap();
            s.push_str(&put(&a[0], "source"));
            s.push_str("}else{");
            s.push_str(&put(&Operand::Reg(0, w), "destination"));
            s.push('}');
        }
        return s;
    }
    if matches!(op, "rol" | "ror" | "rcl" | "rcr" | "shld" | "shrd") {
        ea(&mut s, &a[0]);
        let double = matches!(op, "shld" | "shrd");
        let through_carry = matches!(op, "rcl" | "rcr");
        let count = read(&a[if double { 2 } else { 1 }]);
        writeln!(
            s,
            "uint64_t a={},v=a;unsigned count=(unsigned){count}&{};",
            read(&a[0]),
            if w == 64 { 63 } else { 31 }
        )
        .unwrap();
        if double {
            writeln!(s, "uint64_t source={};", read(&a[1])).unwrap();
        }
        let iterations = if through_carry && w < 32 {
            writeln!(s, "unsigned rotations=count%{};", w + 1).unwrap();
            "rotations"
        } else {
            "count"
        };
        writeln!(
            s,
            "if({iterations}){{unsigned carry={},overflow=m->of;",
            if through_carry { "m->cf" } else { "0" }
        )
        .unwrap();
        let sign = 1u64 << (w - 1);
        let mask = mask(w);
        let step = match op {
            "rol" => {
                format!("carry=(v&UINT64_C(0x{sign:x}))!=0;v=((v<<1)|carry)&UINT64_C(0x{mask:x});")
            }
            "ror" => format!("carry=v&1;v=(v>>1)|((uint64_t)carry<<{});", w - 1),
            "rcl" => format!(
                "unsigned next_carry=(v&UINT64_C(0x{sign:x}))!=0;v=((v<<1)|carry)&UINT64_C(0x{mask:x});carry=next_carry;"
            ),
            "rcr" => format!(
                "unsigned next_carry=v&1;v=(v>>1)|((uint64_t)carry<<{});carry=next_carry;",
                w - 1
            ),
            "shld" => format!(
                "carry=(v&UINT64_C(0x{sign:x}))!=0;v=((v<<1)|((source&UINT64_C(0x{sign:x}))!=0))&UINT64_C(0x{mask:x});source=(source<<1)&UINT64_C(0x{mask:x});"
            ),
            _ => format!("carry=v&1;v=(v>>1)|((source&1)<<{});source>>=1;", w - 1),
        };
        writeln!(s, "for(unsigned j=0;j<{iterations};j++){{{step}}}").unwrap();
        if double {
            writeln!(s, "rosemary_flags(m,0,0,v,{w},0);").unwrap();
        }
        let of = match op {
            "rol" | "rcl" => format!("((v>>{})&1)^carry", w - 1),
            "ror" | "rcr" => format!("((v>>{})^(v>>{}))&1", w - 1, w - 2),
            _ => format!("((a^v)>>{})&1", w - 1),
        };
        writeln!(s, "m->cf=carry;m->of=count==1?({of}):overflow;}}").unwrap();
        s.push_str(&put(&a[0], "v"));
        return s;
    }
    if matches!(op, "adc" | "sbb") {
        ea(&mut s, &a[0]);
        writeln!(
            s,
            "uint64_t a={},b={},carry=m->cf;",
            read(&a[0]),
            read(&a[1])
        )
        .unwrap();
        let symbol = if op == "adc" { "+" } else { "-" };
        writeln!(
            s,
            "uint64_t v=(a {symbol} b {symbol} carry)&UINT64_C(0x{:x});",
            mask(w)
        )
        .unwrap();
        s.push_str(&put(&a[0], "v"));
        writeln!(s, "rosemary_flags(m,a,b,v,{w},0);").unwrap();
        if op == "adc" {
            writeln!(s,"uint64_t sum=(a+b)&UINT64_C(0x{:x});m->cf=sum<a||v<sum;m->of=((~(a^b)&(a^v))>>(w-1))&1;",mask(w)).unwrap();
        } else {
            s.push_str("m->cf=a<b||(a==b&&carry);m->of=((a^b)&(a^v))>>(w-1)&1;");
        }
        // Replace generator-local width in the emitted expression.
        return s.replace("(w-1)", &format!("{}", w - 1));
    }
    if op == "xchg" {
        let memory = a.iter().find(|o| matches!(o, Operand::Mem { .. }));
        if let Some(o) = memory {
            ea(&mut s, o);
        }
        writeln!(s, "uint64_t left={},right={};", read(&a[0]), read(&a[1])).unwrap();
        s.push_str(&put(&a[0], "right"));
        s.push_str(&put(&a[1], "left"));
        return s;
    }
    if matches!(op, "mul" | "imul") {
        writeln!(
            s,
            "uint64_t a=m->r[0]&UINT64_C(0x{:x}),b={},lo,hi;",
            mask(w),
            read(&a[0])
        )
        .unwrap();
        if w == 64 {
            s.push_str("rosemary_mulwide(a,b,&lo,&hi);\n");
        } else {
            writeln!(
                s,
                "lo=a*b;hi=(lo>>{w})&UINT64_C(0x{:x});lo&=UINT64_C(0x{:x});",
                mask(w),
                mask(w)
            )
            .unwrap();
        }
        if op == "imul" {
            writeln!(s,"hi=(hi-((a>>(w-1))?b:0)-((b>>(w-1))?a:0))&UINT64_C(0x{:x});m->cf=m->of=hi!=((lo>>(w-1))?UINT64_C(0x{:x}):0);",mask(w),mask(w)).unwrap();
        } else {
            s.push_str("m->cf=m->of=hi!=0;");
        }
        if w == 8 {
            s.push_str(&put(&accumulator(w), "(hi<<8)|lo"));
        } else {
            s.push_str(&put(&Operand::Reg(0, w), "lo"));
            s.push_str(&put(&high_register(w), "hi"));
        }
        return s.replace("(w-1)", &format!("{}", w - 1));
    }
    if matches!(op, "div" | "idiv") {
        let hi = if w == 8 {
            "(m->r[0]>>8)".into()
        } else {
            "m->r[2]".to_string()
        };
        writeln!(
            s,
            "uint64_t lo=m->r[0]&UINT64_C(0x{:x}),hi={hi}&UINT64_C(0x{:x}),den={},q,r;",
            mask(w),
            mask(w),
            read(&a[0])
        )
        .unwrap();
        writeln!(
            s,
            "if(!rosemary_divwide(hi,lo,den,{w},{},&q,&r)) return 2;",
            u8::from(op == "idiv")
        )
        .unwrap();
        if w == 8 {
            s.push_str(&put(&accumulator(w), "(r<<8)|q"));
        } else {
            s.push_str(&put(&Operand::Reg(0, w), "q"));
            s.push_str(&put(&high_register(w), "r"));
        }
        return s;
    }
    if matches!(op, "bt" | "bts" | "btr" | "btc") {
        let value = if matches!(a[0], Operand::Mem { .. }) {
            writeln!(s, "uint64_t ea={};", bit_address(&a[0], &a[1])).unwrap();
            format!("m->load(m->ctx,ea,{})", w / 8)
        } else {
            read(&a[0])
        };
        writeln!(
            s,
            "uint64_t a={value},bit=UINT64_C(1)<<({}&{});m->cf=(a&bit)!=0;",
            read(&a[1]),
            w - 1
        )
        .unwrap();
        if op != "bt" {
            let v = match op {
                "bts" => "a|bit",
                "btr" => "a&~bit",
                _ => "a^bit",
            };
            s.push_str(&put(&a[0], v));
        }
        return s;
    }
    if op == "bswap" {
        writeln!(
            s,
            "uint64_t a={},v=0;for(unsigned j=0;j<{};j++) v|=((a>>(8*j))&255)<<(8*({}-j));",
            read(&a[0]),
            w / 8,
            w / 8 - 1
        )
        .unwrap();
        s.push_str(&put(&a[0], "v"));
        return s;
    }
    let src = read(&a[1]);
    writeln!(s, "uint64_t a={src},v=0;").unwrap();
    if op == "popcnt" {
        s.push_str("m->zf=a==0;m->cf=m->of=m->sf=m->pf=0;while(a){a&=a-1;v++;}");
        s.push_str(&put(&a[0], "v"));
        return s;
    }
    match op {
        "bsf" | "tzcnt" => writeln!(s, "while(v<{w}&&!(a&(UINT64_C(1)<<v)))v++;").unwrap(),
        _ => writeln!(s, "while(v<{w}&&!(a&(UINT64_C(1)<<({}-v))))v++;", w - 1).unwrap(),
    }
    if matches!(op, "bsf" | "bsr") {
        s.push_str("m->zf=a==0;if(a){");
        let v = if op == "bsr" {
            format!("{}-v", w - 1)
        } else {
            "v".into()
        };
        s.push_str(&put(&a[0], &v));
        s.push('}');
    } else {
        s.push_str("m->cf=a==0;m->zf=v==0;");
        s.push_str(&put(&a[0], "v"));
    }
    s
}
pub(crate) fn transfer(i: &Instruction, mut s: State) -> State {
    let op = i.op.as_str();
    let a = &i.operands;
    if matches!(op, "clc" | "stc" | "cmc") {
        s.flags.cf = match op {
            "clc" => Some(false),
            "stc" => Some(true),
            _ => s.flags.cf.map(|v| !v),
        };
        return s;
    }
    if matches!(op, "cwd" | "cdq" | "cqo") {
        let w = match op {
            "cwd" => 16,
            "cdq" => 32,
            _ => 64,
        };
        s.write(
            &Operand::Reg(2, w),
            s.read(&Operand::Reg(0, w)).map(|v| {
                if v & (1u64 << (w - 1)) != 0 {
                    mask(w)
                } else {
                    0
                }
            }),
        );
        return s;
    }
    let w = a[0].width();
    if matches!(op, "xadd" | "cmpxchg") {
        let dst = s.read(&a[0]);
        let src = s.read(&a[1]);
        let acc = s.read(&Operand::Reg(0, w));
        if op == "xadd" {
            let value = dst.zip(src).map(|(a, b)| a.wrapping_add(b) & mask(w));
            s.write(&a[1], dst);
            s.write(&a[0], value);
            s.flags = crate::dataflow::arithmetic_flags(dst, src, value, w, false);
        } else {
            let value = acc.zip(dst).map(|(a, b)| a.wrapping_sub(b) & mask(w));
            let flags = crate::dataflow::arithmetic_flags(acc, dst, value, w, true);
            match acc.zip(dst).map(|(a, b)| a == b) {
                Some(true) => s.write(&a[0], src),
                Some(false) => s.write(&Operand::Reg(0, w), dst),
                None => {
                    let mut success = s;
                    let mut fail = s;
                    success.write(&a[0], src);
                    fail.write(&Operand::Reg(0, w), dst);
                    for j in 0..16 {
                        if success.registers[j] != fail.registers[j] {
                            success.registers[j] = None;
                        }
                    }
                    s = success;
                }
            }
            s.flags = flags;
        }
        return s;
    }
    if matches!(op, "rol" | "ror" | "rcl" | "rcr" | "shld" | "shrd") {
        let double = matches!(op, "shld" | "shrd");
        let through_carry = matches!(op, "rcl" | "rcr");
        let masked_count = s
            .read(&a[if double { 2 } else { 1 }])
            .map(|v| v & if w == 64 { 63 } else { 31 });
        let count = masked_count.map(|v| {
            if through_carry && w < 32 {
                v % (w as u64 + 1)
            } else {
                v
            }
        });
        let old = s.read(&a[0]);
        let mut v = old;
        let mut source = if double { s.read(&a[1]) } else { None };
        let mut carry = if through_carry { s.flags.cf } else { None };
        if let Some(count) = count {
            if count == 0 {
                s.write(&a[0], v);
                return s;
            }
            for _ in 0..count {
                let incoming_carry = carry;
                carry = v.map(|v| {
                    if matches!(op, "rol" | "rcl" | "shld") {
                        v >> (w - 1) != 0
                    } else {
                        v & 1 != 0
                    }
                });
                v = match op {
                    "rol" => v.map(|v| ((v << 1) | (v >> (w - 1))) & mask(w)),
                    "ror" => v.map(|v| (v >> 1) | ((v & 1) << (w - 1))),
                    "rcl" => v
                        .zip(incoming_carry)
                        .map(|(v, c)| ((v << 1) | u64::from(c)) & mask(w)),
                    "rcr" => v
                        .zip(incoming_carry)
                        .map(|(v, c)| (v >> 1) | (u64::from(c) << (w - 1))),
                    "shld" => v
                        .zip(source)
                        .map(|(v, src)| ((v << 1) | (src >> (w - 1))) & mask(w)),
                    _ => v
                        .zip(source)
                        .map(|(v, src)| (v >> 1) | ((src & 1) << (w - 1))),
                };
                if double {
                    source = source.map(|v| {
                        if op == "shld" {
                            (v << 1) & mask(w)
                        } else {
                            v >> 1
                        }
                    });
                }
            }
            let of = if masked_count == Some(1) {
                match op {
                    "rol" | "rcl" => v.zip(carry).map(|(v, c)| (v >> (w - 1) != 0) != c),
                    "ror" | "rcr" => v.map(|v| ((v >> (w - 1)) ^ (v >> (w - 2))) & 1 != 0),
                    _ => old.zip(v).map(|(a, b)| (a ^ b) >> (w - 1) != 0),
                }
            } else {
                None
            };
            if double {
                s.flags = result_flags(v, w);
            }
            s.flags.cf = carry;
            s.flags.of = of;
            if double && count > w as u64 {
                v = None;
                s.flags = Flags::default();
            }
        } else {
            v = None;
            if double {
                s.flags = Flags::default();
            } else {
                s.flags.cf = None;
                s.flags.of = None;
            }
        }
        s.write(&a[0], v);
        return s;
    }
    if matches!(op, "adc" | "sbb") {
        let value = s.read(&a[0]).zip(s.read(&a[1])).zip(s.flags.cf);
        let v = value.map(|((a, b), c)| {
            if op == "adc" {
                a.wrapping_add(b).wrapping_add(c as u64) & mask(w)
            } else {
                a.wrapping_sub(b).wrapping_sub(c as u64) & mask(w)
            }
        });
        let mut f = result_flags(v, w);
        if let Some((((a, b), c), v)) = value.zip(v) {
            f.cf = Some(if op == "adc" {
                a as u128 + b as u128 + c as u128 > mask(w) as u128
            } else {
                (a as u128) < b as u128 + c as u128
            });
            let sign = 1u64 << (w - 1);
            f.of = Some(if op == "adc" {
                !(a ^ b) & (a ^ v) & sign != 0
            } else {
                (a ^ b) & (a ^ v) & sign != 0
            });
        }
        s.write(&a[0], v);
        s.flags = f;
        return s;
    }
    if op == "xchg" {
        let left = s.read(&a[0]);
        let right = s.read(&a[1]);
        s.write(&a[0], right);
        s.write(&a[1], left);
        return s;
    }
    if matches!(op, "mul" | "imul") {
        let pair = s.read(&Operand::Reg(0, w)).zip(s.read(&a[0]));
        let products = pair.map(|(a, b)| {
            let p = (a as u128) * (b as u128);
            let lo = p as u64 & mask(w);
            let mut hi = (p >> w) as u64 & mask(w);
            if op == "imul" {
                hi = hi
                    .wrapping_sub(if a >> (w - 1) != 0 { b } else { 0 })
                    .wrapping_sub(if b >> (w - 1) != 0 { a } else { 0 })
                    & mask(w);
            }
            let overflow = if op == "imul" {
                hi != if lo >> (w - 1) != 0 { mask(w) } else { 0 }
            } else {
                hi != 0
            };
            (lo, hi, overflow)
        });
        if w == 8 {
            s.write(&accumulator(w), products.map(|(lo, hi, _)| (hi << 8) | lo));
        } else {
            s.write(&Operand::Reg(0, w), products.map(|p| p.0));
            s.write(&high_register(w), products.map(|p| p.1));
        }
        let cf = products.map(|p| p.2);
        s.flags = Flags {
            cf,
            of: cf,
            ..Default::default()
        };
        return s;
    }
    if matches!(op, "div" | "idiv") {
        let numerator = if w == 8 {
            s.read(&Operand::Reg(0, 16)).map(u128::from)
        } else {
            s.read(&Operand::Reg(0, w))
                .zip(s.read(&high_register(w)))
                .map(|(lo, hi)| ((hi as u128) << w) | lo as u128)
        };
        let result = numerator.zip(s.read(&a[0])).and_then(|(n, d)| {
            if op == "div" {
                let q = n.checked_div(d as u128)?;
                let r = n.checked_rem(d as u128)?;
                if q > mask(w) as u128 {
                    return None;
                }
                Some((q as u64, r as u64))
            } else {
                let n = if w == 64 {
                    n as i128
                } else {
                    ((n << (128 - 2 * w)) as i128) >> (128 - 2 * w)
                };
                let d = ((d << (64 - w)) as i64 >> (64 - w)) as i128;
                let q = n.checked_div(d)?;
                let r = n.checked_rem(d)?;
                let sign = 1i128 << (w - 1);
                if q < -sign || q >= sign {
                    return None;
                }
                Some((q as u64 & mask(w), r as u64 & mask(w)))
            }
        });
        if w == 8 {
            s.write(&accumulator(w), result.map(|(q, r)| (r << 8) | q));
        } else {
            s.write(&Operand::Reg(0, w), result.map(|v| v.0));
            s.write(&high_register(w), result.map(|v| v.1));
        }
        s.flags = Flags::default();
        return s;
    }
    if matches!(op, "bt" | "bts" | "btr" | "btc") {
        let pair = s.read(&a[0]).zip(s.read(&a[1]));
        let cf = pair.map(|(v, b)| v & (1u64 << (b & (w as u64 - 1))) != 0);
        if op != "bt" {
            s.write(
                &a[0],
                pair.map(|(v, b)| {
                    let bit = 1u64 << (b & (w as u64 - 1));
                    match op {
                        "bts" => v | bit,
                        "btr" => v & !bit,
                        _ => v ^ bit,
                    }
                }),
            );
        }
        s.flags = Flags {
            cf,
            zf: s.flags.zf,
            ..Default::default()
        };
        return s;
    }
    if op == "bswap" {
        s.write(
            &a[0],
            s.read(&a[0]).map(|v| {
                if w == 32 {
                    (v as u32).swap_bytes() as u64
                } else {
                    v.swap_bytes()
                }
            }),
        );
        return s;
    }
    let input = s.read(&a[1]);
    let value = input.and_then(|v| {
        if v == 0 && matches!(op, "bsf" | "bsr") {
            None
        } else {
            Some(match op {
                "bsf" | "tzcnt" => (v.trailing_zeros() as u64).min(w as u64),
                "bsr" => 63 - v.leading_zeros() as u64,
                "popcnt" => v.count_ones() as u64,
                _ => v.leading_zeros() as u64 - (64 - w) as u64,
            })
        }
    });
    s.write(&a[0], value);
    s.flags = if op == "popcnt" {
        Flags {
            zf: input.map(|v| v == 0),
            cf: Some(false),
            of: Some(false),
            sf: Some(false),
            pf: Some(false),
        }
    } else if matches!(op, "bsf" | "bsr") {
        Flags {
            zf: input.map(|v| v == 0),
            ..Default::default()
        }
    } else {
        Flags {
            cf: input.map(|v| v == 0),
            zf: value.map(|v| v == 0),
            ..Default::default()
        }
    };
    s
}
