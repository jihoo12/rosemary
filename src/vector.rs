//! Portable SSE through SSE4.1 bit-vector semantics. No host SIMD dependency in output.
use crate::{
    emit::{address, put, read},
    ir::{Instruction, Operand},
};
use std::fmt::Write;
const MOVES: &[&str] = &["movups", "movaps", "movupd", "movapd", "movdqu", "movdqa"];
const HALF_MOVES: &[&str] = &["movlps", "movhps", "movlpd", "movhpd"];
const DUPLICATES: &[&str] = &["movddup", "movsldup", "movshdup"];
const LOGIC: &[&str] = &[
    "pxor", "xorps", "xorpd", "pand", "andps", "andpd", "pandn", "andnps", "andnpd", "por", "orps",
    "orpd",
];
pub(crate) fn lanes(op: &str) -> Option<(u8, &str)> {
    match op {
        "pcmpeqq" => return Some((64, "pcmpeq")),
        "pabsb" => return Some((8, "pabs")),
        "pabsw" => return Some((16, "pabs")),
        "pabsd" => return Some((32, "pabs")),
        "psignb" => return Some((8, "psign")),
        "psignw" => return Some((16, "psign")),
        "psignd" => return Some((32, "psign")),
        "pminub" => return Some((8, "pminu")),
        "pminsb" => return Some((8, "pmins")),
        "pminsd" => return Some((32, "pmins")),
        "pminuw" => return Some((16, "pminu")),
        "pminud" => return Some((32, "pminu")),
        "pmaxub" => return Some((8, "pmaxu")),
        "pmaxsb" => return Some((8, "pmaxs")),
        "pmaxsd" => return Some((32, "pmaxs")),
        "pmaxuw" => return Some((16, "pmaxu")),
        "pmaxud" => return Some((32, "pmaxu")),
        "pminsw" => return Some((16, "pmins")),
        "pmaxsw" => return Some((16, "pmaxs")),
        "paddusb" => return Some((8, "paddus")),
        "paddusw" => return Some((16, "paddus")),
        "psubusb" => return Some((8, "psubus")),
        "psubusw" => return Some((16, "psubus")),
        "paddsb" => return Some((8, "padds")),
        "paddsw" => return Some((16, "padds")),
        "psubsb" => return Some((8, "psubs")),
        "psubsw" => return Some((16, "psubs")),
        "pavgb" => return Some((8, "pavg")),
        "pavgw" => return Some((16, "pavg")),
        "pmullw" => return Some((16, "pmull")),
        "pmulld" => return Some((32, "pmull")),
        "pmulhw" => return Some((16, "pmulh")),
        "pmulhuw" => return Some((16, "pmulhu")),
        "pmulhrsw" => return Some((16, "pmulhrs")),
        _ => {}
    }
    for prefix in ["padd", "psub", "pcmpeq", "pcmpgt", "psrl", "psll", "psra"] {
        if let Some(suffix) = op.strip_prefix(prefix) {
            let bits = match suffix {
                "b" => 8,
                "w" => 16,
                "d" => 32,
                "q" => 64,
                _ => return None,
            };
            if matches!(prefix, "psrl" | "psll" | "psra") && bits == 8 {
                return None;
            }
            if prefix == "psra" && bits == 64 {
                return None;
            }
            if matches!(prefix, "pcmpeq" | "pcmpgt") && bits == 64 {
                return None;
            }
            return Some((bits, prefix));
        }
    }
    None
}
/// Lane width, subtraction, and signed saturation for horizontal pairs.
pub(crate) fn horizontal(op: &str) -> Option<(u8, bool, bool)> {
    Some(match op {
        "phaddw" => (16, false, false),
        "phaddd" => (32, false, false),
        "phaddsw" => (16, false, true),
        "phsubw" => (16, true, false),
        "phsubd" => (32, true, false),
        "phsubsw" => (16, true, true),
        _ => return None,
    })
}
pub(crate) fn unpack(op: &str) -> Option<(u8, bool)> {
    let (rest, high) = if let Some(s) = op.strip_prefix("punpckl") {
        (s, false)
    } else {
        (op.strip_prefix("punpckh")?, true)
    };
    Some((
        match rest {
            "bw" => 8,
            "wd" => 16,
            "dq" => 32,
            "qdq" => 64,
            _ => return None,
        },
        high,
    ))
}
pub(crate) fn pack(op: &str) -> Option<(u8, bool)> {
    match op {
        "packsswb" => Some((16, false)),
        "packssdw" => Some((32, false)),
        "packuswb" => Some((16, true)),
        _ => None,
    }
}
pub(crate) fn recognized(op: &str) -> bool {
    MOVES.contains(&op)
        || HALF_MOVES.contains(&op)
        || DUPLICATES.contains(&op)
        || LOGIC.contains(&op)
        || lanes(op).is_some()
        || horizontal(op).is_some()
        || unpack(op).is_some()
        || pack(op).is_some()
        || matches!(
            op,
            "movd"
                | "movq"
                | "movss"
                | "movsd"
                | "pshufd"
                | "pshuflw"
                | "pshufhw"
                | "shufps"
                | "shufpd"
                | "pmovmskb"
                | "movmskps"
                | "movmskpd"
                | "pmuludq"
                | "pmuldq"
                | "ptest"
                | "pmaddwd"
                | "pmaddubsw"
                | "palignr"
                | "psadbw"
                | "pshufb"
                | "pinsrw"
                | "pextrw"
                | "pslldq"
                | "psrldq"
                | "movlhps"
                | "movhlps"
        )
}
pub(crate) fn valid(i: &Instruction) -> Option<bool> {
    let op = i.op.as_str();
    if !recognized(op) {
        return None;
    }
    let a = &i.operands;
    let vector = |o: &Operand| matches!(o, Operand::Vec(_) | Operand::Mem { width: 128, .. });
    let scalar =
        |o: &Operand, w| matches!(o,Operand::Reg(_,sw)|Operand::Mem{width:sw,..} if *sw==w);
    let binary = matches!(a.as_slice(),[Operand::Vec(_),src] if vector(src));
    Some(if MOVES.contains(&op) {
        binary
            || matches!(
                a.as_slice(),
                [Operand::Mem { width: 128, .. }, Operand::Vec(_)]
            )
    } else if HALF_MOVES.contains(&op) {
        matches!(
            a.as_slice(),
            [Operand::Vec(_), Operand::Mem { width: 64, .. }]
                | [Operand::Mem { width: 64, .. }, Operand::Vec(_)]
        )
    } else if op == "movddup" {
        matches!(
            a.as_slice(),
            [
                Operand::Vec(_),
                Operand::Vec(_) | Operand::Mem { width: 64, .. }
            ]
        )
    } else if matches!(op, "movd" | "movq") {
        let w = if op == "movd" { 32 } else { 64 };
        matches!(a.as_slice(),[Operand::Vec(_),src] if scalar(src,w))
            || matches!(a.as_slice(),[dst,Operand::Vec(_)] if scalar(dst,w))
            || (op == "movq" && matches!(a.as_slice(), [Operand::Vec(_), Operand::Vec(_)]))
    } else if matches!(op, "movss" | "movsd") {
        let w = if op == "movss" { 32 } else { 64 };
        matches!(a.as_slice(), [Operand::Vec(_), Operand::Vec(_)])
            || matches!(a.as_slice(),[Operand::Vec(_),Operand::Mem{width,..}] if *width==w)
            || matches!(a.as_slice(),[Operand::Mem{width,..},Operand::Vec(_)] if *width==w)
    } else if matches!(op, "pmovmskb" | "movmskps" | "movmskpd") {
        matches!(a.as_slice(), [Operand::Reg(_, 32), Operand::Vec(_)])
    } else if op == "pextrw" {
        matches!(
            a.as_slice(),
            [Operand::Reg(_, 32), Operand::Vec(_), Operand::Imm(_, 8)]
        )
    } else if op == "pinsrw" {
        matches!(a.as_slice(),[Operand::Vec(_),src,Operand::Imm(_,8)] if matches!(src,Operand::Reg(_,32)|Operand::Mem{width:16,..}))
    } else if matches!(
        op,
        "pshufd" | "pshuflw" | "pshufhw" | "shufps" | "shufpd" | "palignr"
    ) {
        matches!(a.as_slice(),[Operand::Vec(_),src,Operand::Imm(_,8)] if vector(src))
    } else if matches!(op, "pslldq" | "psrldq") {
        matches!(a.as_slice(), [Operand::Vec(_), Operand::Imm(_, 8)])
    } else if lanes(op).is_some_and(|(_, k)| matches!(k, "psll" | "psrl" | "psra")) {
        binary || matches!(a.as_slice(), [Operand::Vec(_), Operand::Imm(_, 8)])
    } else if matches!(op, "movlhps" | "movhlps") {
        matches!(a.as_slice(), [Operand::Vec(_), Operand::Vec(_)])
    } else {
        binary
    })
}
pub(crate) const HELPERS: &str = r#"
static inline uint64_t rosemary_vmask(unsigned bits) {
    return bits == 64 ? UINT64_MAX : (UINT64_C(1) << bits)-1;
}
/* Signed saturation is used only for validated 8/16-bit lanes. Widen before
   arithmetic so no signed overflow or out-of-range signed cast is required. */
static inline uint64_t rosemary_vsat_signed(uint64_t a,uint64_t b,unsigned bits,unsigned subtract) {
    int64_t sign=INT64_C(1)<<(bits-1);
    int64_t left=(int64_t)(a^(uint64_t)sign)-sign;
    int64_t right=(int64_t)(b^(uint64_t)sign)-sign;
    int64_t value=subtract?left-right:left+right;
    if(value < -sign) value=-sign;
    if(value >= sign) value=sign-1;
    return (uint64_t)value;
}
static inline uint64_t rosemary_vget(rosemary_vec v, unsigned lane, unsigned bits) {
    unsigned shift = lane * bits;
    return ((shift < 64 ? v.lo : v.hi) >> (shift % 64)) & rosemary_vmask(bits);
}
static inline void rosemary_vset(rosemary_vec *v, unsigned lane, unsigned bits, uint64_t value) {
    unsigned shift = lane * bits;
    uint64_t *word = shift < 64 ? &v->lo : &v->hi;
    uint64_t mask = rosemary_vmask(bits);
    *word = (*word & ~(mask << (shift % 64))) | ((value & mask) << (shift % 64));
}
static inline rosemary_vec rosemary_vload(rosemary_machine *m, uint64_t a) {
    rosemary_vec v = {m->load(m->ctx,a,8),m->load(m->ctx,a+8,8)}; return v;
}
static inline void rosemary_vstore(rosemary_machine *m, uint64_t a, rosemary_vec v) {
    m->store(m->ctx,a,8,v.lo); m->store(m->ctx,a+8,8,v.hi);
}
static inline uint64_t rosemary_vshift(uint64_t value, uint64_t count, unsigned bits, unsigned kind) {
    uint64_t mask = rosemary_vmask(bits), sign = UINT64_C(1) << (bits-1);
    if (count >= bits) return kind == 2 && (value & sign) ? mask : 0;
    if (!count) return value;
    if (kind == 0) return (value << count) & mask;
    return (value >> count) | (kind == 2 && (value & sign) ? mask ^ (mask >> count) : 0);
}
"#;
fn vread(o: &Operand) -> String {
    match o {
        Operand::Vec(r) => format!("m->x[{r}]"),
        Operand::Mem { width: 128, .. } => format!("rosemary_vload(m,{})", address(o)),
        _ => format!("(rosemary_vec){{{},0}}", read(o)),
    }
}
fn vput(o: &Operand, v: &str) -> String {
    match o {
        Operand::Vec(r) => format!("m->x[{r}] = {v};"),
        Operand::Mem { width: 128, .. } => format!("rosemary_vstore(m,{}, {v});", address(o)),
        _ => {
            let mut s = String::new();
            if matches!(o, Operand::Mem { .. }) {
                write!(s, "uint64_t ea = {}; ", address(o)).unwrap();
            }
            s.push_str(&put(o, &format!("({v}).lo")));
            s
        }
    }
}
pub(crate) fn emit(i: &Instruction) -> String {
    let op = i.op.as_str();
    let a = &i.operands;
    let mut s = String::new();
    if op == "ptest" {
        writeln!(s,"rosemary_vec a={},b={};m->zf=((a.lo&b.lo)|(a.hi&b.hi))==0;m->cf=((~a.lo&b.lo)|(~a.hi&b.hi))==0;m->of=m->sf=m->pf=0;",vread(&a[0]),vread(&a[1])).unwrap();
        return s;
    }
    if DUPLICATES.contains(&op) {
        writeln!(s, "rosemary_vec source={};", vread(&a[1])).unwrap();
        if op == "movddup" {
            s.push_str("rosemary_vec v={source.lo,source.lo};");
        } else {
            let offset = u8::from(op == "movshdup");
            writeln!(s, "rosemary_vec v={{0,0}};for(unsigned j=0;j<4;j++) rosemary_vset(&v,j,32,rosemary_vget(source,(j&2)+{offset},32));").unwrap();
        }
        s.push_str(&vput(&a[0], "v"));
        return s;
    }
    if HALF_MOVES.contains(&op) {
        let high = matches!(op, "movhps" | "movhpd");
        if matches!(a[0], Operand::Vec(_)) {
            writeln!(
                s,
                "rosemary_vec v={};v.{}={};",
                vread(&a[0]),
                if high { "hi" } else { "lo" },
                read(&a[1])
            )
            .unwrap();
        } else {
            writeln!(s, "rosemary_vec v={};", vread(&a[1])).unwrap();
            if high {
                s.push_str("v.lo=v.hi;");
            }
        }
        s.push_str(&vput(&a[0], "v"));
        return s;
    }
    if matches!(op, "movd" | "movq" | "movss" | "movsd") {
        writeln!(s, "rosemary_vec source = {};", vread(&a[1])).unwrap();
        if matches!(a[0], Operand::Vec(_)) {
            let w = if matches!(op, "movd" | "movss") {
                32
            } else {
                64
            };
            let preserve = matches!(op, "movss" | "movsd") && matches!(a[1], Operand::Vec(_));
            writeln!(
                s,
                "rosemary_vec v = {};",
                if preserve {
                    vread(&a[0])
                } else {
                    "{0,0}".into()
                }
            )
            .unwrap();
            writeln!(
                s,
                "v.lo = (v.lo & ~rosemary_vmask({w})) | (source.lo & rosemary_vmask({w}));"
            )
            .unwrap();
            s.push_str(&vput(&a[0], "v"));
        } else {
            s.push_str(&vput(&a[0], "source"));
        }
        return s;
    }
    if matches!(op, "pmovmskb" | "movmskps" | "movmskpd" | "pextrw") {
        writeln!(s, "rosemary_vec source = {}; uint64_t v = 0;", vread(&a[1])).unwrap();
        if op == "pextrw" {
            writeln!(
                s,
                "v = rosemary_vget(source, (unsigned){} & 7, 16);",
                read(&a[2])
            )
            .unwrap();
        } else {
            let bits = match op {
                "pmovmskb" => 8,
                "movmskps" => 32,
                _ => 64,
            };
            writeln!(s,"for(unsigned j=0;j<{};j++) v |= ((rosemary_vget(source,j,{bits}) >> {}) & 1) << j;",128/bits,bits-1).unwrap();
        }
        s.push_str(&put(&a[0], "v"));
        return s;
    }
    if MOVES.contains(&op) {
        writeln!(s, "rosemary_vec v = {};", vread(&a[1])).unwrap();
        s.push_str(&vput(&a[0], "v"));
        return s;
    }
    if op == "pinsrw" {
        writeln!(
            s,
            "rosemary_vec v = {}; rosemary_vset(&v,(unsigned){} & 7,16,{});",
            vread(&a[0]),
            read(&a[2]),
            read(&a[1])
        )
        .unwrap();
        s.push_str(&vput(&a[0], "v"));
        return s;
    }
    if op == "palignr" {
        writeln!(
            s,
            "rosemary_vec a={},b={},v={{0,0}};unsigned count=(unsigned){};",
            vread(&a[0]),
            vread(&a[1]),
            read(&a[2])
        )
        .unwrap();
        // The source supplies the low half of the 256-bit concatenation.
        // Byte extraction avoids oversized C shifts, including counts >= 32.
        s.push_str("for(unsigned j=0;j<16;j++){unsigned index=j+count;if(index<32)rosemary_vset(&v,j,8,rosemary_vget(index<16?b:a,index&15,8));}\n");
    } else if matches!(op, "pslldq" | "psrldq") {
        writeln!(
            s,
            "rosemary_vec a = {}, v = {{0,0}}; unsigned count = (unsigned){};",
            vread(&a[0]),
            read(&a[1])
        )
        .unwrap();
        let source = if op == "pslldq" { "j-count" } else { "j+count" };
        let guard = if op == "pslldq" {
            "j>=count"
        } else {
            "j+count<16"
        };
        writeln!(s,"for(unsigned j=0;j<16;j++) if(count<16 && {guard}) rosemary_vset(&v,j,8,rosemary_vget(a,{source},8));").unwrap();
    } else if matches!(op, "shufps" | "shufpd") {
        let (bits, stride) = if op == "shufps" { (32, 2) } else { (64, 1) };
        writeln!(
            s,
            "rosemary_vec a={},b={},v={{0,0}};unsigned select=(unsigned){};",
            vread(&a[0]),
            vread(&a[1]),
            read(&a[2])
        )
        .unwrap();
        writeln!(s, "for(unsigned j=0;j<{};j++) rosemary_vset(&v,j,{bits},rosemary_vget(j<{}?a:b,(select>>(j*{stride}))&{},{bits}));",128/bits,64/bits,128/bits-1).unwrap();
    } else if matches!(op, "pshufd" | "pshuflw" | "pshufhw") {
        let bits = if op == "pshufd" { 32 } else { 16 };
        let offset = if op == "pshufhw" { 4 } else { 0 };
        writeln!(
            s,
            "rosemary_vec b = {}, v = b; unsigned select = (unsigned){};",
            vread(&a[1]),
            read(&a[2])
        )
        .unwrap();
        writeln!(s,"for(unsigned j=0;j<4;j++) rosemary_vset(&v,j+{offset},{bits},rosemary_vget(b,((select>>(2*j))&3)+{offset},{bits}));").unwrap();
    } else if matches!(op, "pmaddwd" | "pmaddubsw" | "psadbw" | "pshufb") {
        writeln!(
            s,
            "rosemary_vec a={},b={},v={{0,0}};",
            vread(&a[0]),
            vread(&a[1])
        )
        .unwrap();
        match op {
            "pmaddubsw" => s.push_str("for(unsigned j=0;j<8;j++){int64_t sum=0;for(unsigned k=0;k<2;k++){int64_t left=(int64_t)rosemary_vget(a,2*j+k,8),right=(int64_t)(rosemary_vget(b,2*j+k,8)^UINT64_C(0x80))-INT64_C(0x80);sum+=left*right;}if(sum<(-32768))sum=(-32768);if(sum>32767)sum=32767;rosemary_vset(&v,j,16,(uint64_t)sum);}\n"),
            "pshufb" => s.push_str("for(unsigned j=0;j<16;j++){uint64_t select=rosemary_vget(b,j,8);rosemary_vset(&v,j,8,(select&128)?0:rosemary_vget(a,(unsigned)select&15,8));}\n"),
            "psadbw" => s.push_str("for(unsigned j=0;j<2;j++){uint64_t sum=0;for(unsigned k=0;k<8;k++){uint64_t left=rosemary_vget(a,8*j+k,8),right=rosemary_vget(b,8*j+k,8);sum+=left>right?left-right:right-left;}rosemary_vset(&v,j,64,sum);}\n"),
            _ => s.push_str("for(unsigned j=0;j<4;j++){int64_t sum=0;for(unsigned k=0;k<2;k++){int64_t left=(int64_t)(rosemary_vget(a,2*j+k,16)^UINT64_C(0x8000))-INT64_C(0x8000),right=(int64_t)(rosemary_vget(b,2*j+k,16)^UINT64_C(0x8000))-INT64_C(0x8000);sum+=left*right;}rosemary_vset(&v,j,32,(uint64_t)sum);}\n"),
        }
    } else if let Some((bits, subtract, saturating)) = horizontal(op) {
        let half = 64 / bits;
        let symbol = if subtract { "-" } else { "+" };
        writeln!(
            s,
            "rosemary_vec a={},b={},v={{0,0}};",
            vread(&a[0]),
            vread(&a[1])
        )
        .unwrap();
        writeln!(s,"for(unsigned j=0;j<{};j++){{rosemary_vec source=j<{half}?a:b;unsigned index=2*(j%{half});uint64_t left=rosemary_vget(source,index,{bits}),right=rosemary_vget(source,index+1,{bits});",128/bits).unwrap();
        if saturating {
            s.push_str(&format!("int64_t value=((int64_t)(left^UINT64_C(0x8000))-INT64_C(0x8000)){symbol}((int64_t)(right^UINT64_C(0x8000))-INT64_C(0x8000));if(value<(-32768))value=(-32768);if(value>32767)value=32767;rosemary_vset(&v,j,16,(uint64_t)value);"));
        } else {
            writeln!(s, "rosemary_vset(&v,j,{bits},left{symbol}right);").unwrap();
        }
        s.push_str("}\n");
    } else if let Some((bits, unsigned)) = pack(op) {
        let output = bits / 2;
        let low = if unsigned { 0 } else { -(1i64 << (output - 1)) };
        let high = (1i64 << (output - u8::from(!unsigned))) - 1;
        let sign = 1u64 << (bits - 1);
        writeln!(
            s,
            "rosemary_vec a={},b={},v={{0,0}};",
            vread(&a[0]),
            vread(&a[1])
        )
        .unwrap();
        // Widen every signed source before clamping; avoid out-of-range signed
        // conversions and snapshot both sources for destination/source aliases.
        writeln!(s, "for(unsigned j=0;j<{};j++){{uint64_t raw=rosemary_vget(j<{}?a:b,j%{},{bits});int64_t value=(int64_t)(raw^UINT64_C(0x{sign:x}))-INT64_C(0x{sign:x});if(value<({low}))value=({low});if(value>{high})value={high};rosemary_vset(&v,j,{output},(uint64_t)value);}}",128/output,128/bits,128/bits).unwrap();
    } else if let Some((bits, kind)) = lanes(op) {
        if kind == "pabs" {
            writeln!(s, "rosemary_vec source={},v={{0,0}};", vread(&a[1])).unwrap();
            writeln!(s, "for(unsigned j=0;j<{};j++){{uint64_t raw=rosemary_vget(source,j,{bits});rosemary_vset(&v,j,{bits},(raw&(UINT64_C(1)<<{}))?UINT64_C(0)-raw:raw);}}", 128/bits,bits-1).unwrap();
            s.push_str(&vput(&a[0], "v"));
            return s;
        }
        writeln!(s, "rosemary_vec a = {}, v = {{0,0}};", vread(&a[0])).unwrap();
        if matches!(kind, "psll" | "psrl" | "psra") {
            let count = if matches!(a[1], Operand::Imm(..)) {
                read(&a[1])
            } else {
                format!("({}).lo", vread(&a[1]))
            };
            let k = match kind {
                "psll" => 0,
                "psrl" => 1,
                _ => 2,
            };
            writeln!(s,"uint64_t count = {count}; for(unsigned j=0;j<{};j++) rosemary_vset(&v,j,{bits},rosemary_vshift(rosemary_vget(a,j,{bits}),count,{bits},{k}));",128/bits).unwrap();
        } else {
            writeln!(s, "rosemary_vec b = {};", vread(&a[1])).unwrap();
            let expression = match kind {
                "psign" => format!("right == 0 ? 0 : (right & (UINT64_C(1)<<{})) ? UINT64_C(0)-left : left", bits-1),
                "padd" => "left + right".into(),
                "psub" => "left - right".into(),
                "paddus" => format!(
                    "left + right > rosemary_vmask({bits}) ? rosemary_vmask({bits}) : left + right"
                ),
                "psubus" => "left < right ? 0 : left - right".into(),
                "padds" | "psubs" => format!(
                    "rosemary_vsat_signed(left,right,{bits},{})",
                    u8::from(kind == "psubs")
                ),
                "pavg" => "(left + right + 1) >> 1".into(),
                "pmull" => "left * right".into(),
                "pmulhu" => "(left * right) >> 16".into(),
                "pmulh" => "(uint64_t)(((int64_t)(left ^ UINT64_C(0x8000))-INT64_C(0x8000))*((int64_t)(right ^ UINT64_C(0x8000))-INT64_C(0x8000))) >> 16".into(),
                "pmulhrs" => "(uint64_t)(((int64_t)(left ^ UINT64_C(0x8000))-INT64_C(0x8000))*((int64_t)(right ^ UINT64_C(0x8000))-INT64_C(0x8000))+INT64_C(0x4000)) >> 15".into(),
                "pcmpeq" => format!("left == right ? rosemary_vmask({bits}) : 0"),
                "pminu" => "left < right ? left : right".into(),
                "pmaxu" => "left > right ? left : right".into(),
                "pmins" | "pmaxs" => format!(
                    "(left ^ (UINT64_C(1)<<{})) {} (right ^ (UINT64_C(1)<<{})) ? left : right",
                    bits - 1,
                    if kind == "pmins" { "<" } else { ">" },
                    bits - 1
                ),
                _ => format!(
                    "(left ^ (UINT64_C(1)<<{})) > (right ^ (UINT64_C(1)<<{})) ? rosemary_vmask({bits}) : 0",
                    bits - 1,
                    bits - 1
                ),
            };
            writeln!(s,"for(unsigned j=0;j<{};j++) {{uint64_t left=rosemary_vget(a,j,{bits}),right=rosemary_vget(b,j,{bits}); rosemary_vset(&v,j,{bits},{expression});}}",128/bits).unwrap();
        }
    } else {
        writeln!(
            s,
            "rosemary_vec a = {}, b = {}, v = {{0,0}};",
            vread(&a[0]),
            vread(&a[1])
        )
        .unwrap();
        if let Some((bits, high)) = unpack(op) {
            let half = 64 / bits;
            let offset = if high { half } else { 0 };
            writeln!(s,"for(unsigned j=0;j<{half};j++) {{rosemary_vset(&v,2*j,{bits},rosemary_vget(a,j+{offset},{bits}));rosemary_vset(&v,2*j+1,{bits},rosemary_vget(b,j+{offset},{bits}));}}").unwrap();
        } else if op == "pmuldq" {
            // Sign-extend before multiplying; every signed dword product fits i64.
            s.push_str("for(unsigned j=0;j<2;j++){int64_t left=(int64_t)(rosemary_vget(a,2*j,32)^UINT64_C(0x80000000))-INT64_C(0x80000000),right=(int64_t)(rosemary_vget(b,2*j,32)^UINT64_C(0x80000000))-INT64_C(0x80000000);rosemary_vset(&v,j,64,(uint64_t)(left*right));}\n");
        } else if op == "pmuludq" {
            s.push_str("v.lo=(uint64_t)(uint32_t)a.lo*(uint32_t)b.lo; v.hi=(uint64_t)(uint32_t)a.hi*(uint32_t)b.hi;\n");
        } else if op == "movlhps" {
            s.push_str("v.lo=a.lo; v.hi=b.lo;\n");
        } else if op == "movhlps" {
            s.push_str("v.lo=b.hi; v.hi=a.hi;\n");
        } else {
            let sym = if op.starts_with("pxor") || op.starts_with("xor") {
                "^"
            } else if op.starts_with("por") || op.starts_with("or") {
                "|"
            } else {
                "&"
            };
            let inv = if matches!(op, "pandn" | "andnps" | "andnpd") {
                "~"
            } else {
                ""
            };
            writeln!(s, "v.lo={inv}a.lo {sym} b.lo; v.hi={inv}a.hi {sym} b.hi;").unwrap();
        }
    }
    s.push_str(&vput(&a[0], "v"));
    s
}
