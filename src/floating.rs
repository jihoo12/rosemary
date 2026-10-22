//! Scalar SSE comparisons use IEEE bit patterns and the guest's MXCSR.
use crate::{
    emit::read,
    ir::{Instruction, Operand},
};

pub(crate) fn valid(i: &Instruction) -> Option<bool> {
    let width = match i.op.as_str() {
        "comiss" | "ucomiss" => 32,
        "comisd" | "ucomisd" => 64,
        _ => return None,
    };
    Some(
        matches!(i.operands.as_slice(), [Operand::Vec(_), Operand::Vec(_)])
            || matches!(i.operands.as_slice(), [Operand::Vec(_), Operand::Mem {width:w,..}] if *w==width),
    )
}

pub(crate) const HELPERS: &str = r#"
/* Return zero before updating EFLAGS for an unmasked SIMD exception.
   NaN handling has priority over denormal operands, even with masked invalid. */
static inline unsigned rosemary_fp_compare(rosemary_machine *m,uint64_t a,uint64_t b,unsigned bits,unsigned ordered) {
    uint64_t sign=UINT64_C(1)<<(bits-1);
    uint64_t fraction=(UINT64_C(1)<<(bits==32?23:52))-1;
    uint64_t exponent=(sign-1)^fraction,quiet=(fraction+1)>>1;
    a&=sign|(sign-1);b&=sign|(sign-1);
    unsigned nan_a=(a&exponent)==exponent&&(a&fraction)!=0;
    unsigned nan_b=(b&exponent)==exponent&&(b&fraction)!=0;
    unsigned unordered=nan_a||nan_b,exception=0;
    if(unordered) {
        if(ordered||(nan_a&&!(a&quiet))||(nan_b&&!(b&quiet)))exception=1;
    } else {
        unsigned denormal_a=!(a&exponent)&&(a&fraction)!=0;
        unsigned denormal_b=!(b&exponent)&&(b&fraction)!=0;
        if(m->mxcsr&64) {
            if(denormal_a)a&=sign;
            if(denormal_b)b&=sign;
        } else if(denormal_a||denormal_b)exception=2;
    }
    m->mxcsr|=exception;
    if(exception&~(m->mxcsr>>7))return 0;
    uint64_t magnitude_a=a&(sign-1),magnitude_b=b&(sign-1);
    unsigned equal=a==b||(!magnitude_a&&!magnitude_b);
    unsigned less=(a&sign)!=(b&sign)?(a&sign)!=0:(a&sign)?magnitude_a>magnitude_b:magnitude_a<magnitude_b;
    m->zf=unordered||equal;m->pf=unordered;m->cf=unordered||(!equal&&less);m->of=m->sf=0;
    return 1;
}
"#;

pub(crate) fn emit(i: &Instruction) -> String {
    let [Operand::Vec(left), right] = i.operands.as_slice() else {
        unreachable!()
    };
    let source = match right {
        Operand::Vec(r) => format!("m->x[{r}].lo"),
        _ => read(right),
    };
    format!(
        "if(!rosemary_fp_compare(m,m->x[{left}].lo,{source},{},{}))return 2;",
        if i.op.ends_with("ss") { 32 } else { 64 },
        u8::from(i.op.starts_with("comi"))
    )
}
