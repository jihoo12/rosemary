#![cfg(target_arch = "x86_64")]
use rosemary::{
    decode, emit, input,
    ir::{Instruction, Operand, Program},
};
use std::{fmt::Write, fs, process::Command};

fn compile_run(name: &str, c: String) {
    let dir = std::env::temp_dir().join(format!("rosemary-{name}-{}", std::process::id()));
    fs::create_dir_all(&dir).unwrap();
    fs::write(dir.join("test.c"), c).unwrap();
    let out = Command::new("cc")
        .args([
            "-std=c11",
            "-O2",
            "-mno-red-zone",
            "-Wall",
            "-Wextra",
            "-Werror",
        ])
        .arg(dir.join("test.c"))
        .arg("-o")
        .arg(dir.join("test"))
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    assert!(Command::new(dir.join("test")).status().unwrap().success());
    fs::remove_dir_all(dir).unwrap();
}
#[test]
fn wide_multiply_divide_and_guest_division_traps_match_native() {
    let mut p = Program {
        entry: 0,
        instructions: Vec::new(),
    };
    let mut cases = Vec::new();
    for w in [8, 16, 32, 64] {
        for op in ["mul", "imul", "div", "idiv"] {
            let address = p.instructions.len() as u64;
            p.instructions.push(Instruction {
                address,
                len: 1,
                op: op.into(),
                operands: vec![Operand::Reg(1, w)],
            });
            cases.push((address, w, op));
        }
    }
    let mut c = emit::c(&p).unwrap();
    c.push_str(
        r#"
#include <assert.h>
int main(void){
 uint64_t seed=93;
 for(unsigned trial=0;trial<1024;trial++){
  seed=seed*UINT64_C(6364136223846793005)+1;uint64_t low=seed;
  seed=seed*UINT64_C(6364136223846793005)+1;uint64_t high=seed;
  seed=seed*UINT64_C(6364136223846793005)+1;uint64_t divisor=seed;
  if(trial<8){low=trial%2?UINT64_MAX:0;high=trial%3?0:UINT64_MAX;divisor=trial%4?trial%4:0;}
"#,
    );
    for (address, w, op) in cases {
        let suffix = match w {
            8 => "b",
            16 => "w",
            32 => "l",
            _ => "q",
        };
        let operand = match w {
            8 => "%b3",
            16 => "%w3",
            32 => "%k3",
            _ => "%q3",
        };
        writeln!(c,"{{uint64_t mask=rosemary_vmask({w});(void)mask;rosemary_machine m={{0}};m.r[0]=low;m.r[2]=high;m.r[1]=divisor;m.rip={address};").unwrap();
        if matches!(op, "div" | "idiv") {
            writeln!(
                c,
                "unsigned __int128 numerator={};uint64_t den=divisor&mask;unsigned trap=den==0;",
                if w == 8 {
                    "low&65535".into()
                } else {
                    format!("((unsigned __int128)(high&mask)<<{w})|(low&mask)")
                }
            )
            .unwrap();
            if op == "div" {
                c.push_str("if(!trap) trap=numerator/den>mask;\n");
            } else {
                // Build the sign extension without shifting by 128.
                let n = if w == 64 {
                    "(__int128)numerator".into()
                } else {
                    format!("((__int128)(numerator<<{}))>>{}", 128 - 2 * w, 128 - 2 * w)
                };
                writeln!(c,"__int128 sn={n};__int128 sd=(__int128)((den^(UINT64_C(1)<<{}))-(UINT64_C(1)<<{}));",w-1,w-1).unwrap();
                // The unsigned subtraction above needs explicit sign extension for narrow divisors.
                writeln!(c, "sd=(__int128)(int64_t)((den<<(64-{w})))>>(64-{w});").unwrap();
                if w == 64 {
                    c.push_str(
                        "if(sn==(-((__int128)1<<126)-((__int128)1<<126)) && sd==-1) trap=1;\n",
                    );
                }
                writeln!(c,"if(!trap){{__int128 q=sn/sd;__int128 bound=(__int128)1<<{};trap=q< -bound||q>=bound;}}",w-1).unwrap();
            }
            writeln!(c,"int status=rosemary_run(&m,1);if(trap){{assert(status==2&&m.r[0]==low&&m.r[2]==high&&m.rip=={address});}}else{{assert(status==1);").unwrap();
            writeln!(c,"uint64_t left=low,right=high;__asm__ volatile(\"{op}{suffix} {}\":\"+a\"(left),\"+d\"(right):\"c\"(divisor):\"cc\");assert(m.r[0]==left&&m.r[2]==right);}}",operand.replace('3',"2")).unwrap();
        } else {
            writeln!(c,"assert(rosemary_run(&m,1)==1);uint64_t left=low,right=high,flags;__asm__ volatile(\"{op}{suffix} {operand};pushfq;popq %2\":\"+a\"(left),\"+d\"(right),\"=r\"(flags):\"c\"(divisor):\"cc\",\"memory\");assert(m.r[0]==left&&m.r[2]==right&&m.cf==(flags&1)&&m.of==((flags>>11)&1));").unwrap();
        }
        c.push_str("}\n");
    }
    c.push_str("}return 0;}\n");
    compile_run("wide-native", c);
}
#[test]
fn bit_operations_double_shifts_high_bytes_and_atomic_operations_match_native() {
    let mut p = Program {
        entry: 0,
        instructions: Vec::new(),
    };
    let mut cases = Vec::new();
    for w in [16, 32, 64] {
        for op in [
            "bt", "bts", "btr", "btc", "bsf", "bsr", "bswap", "shld", "shrd", "xchg", "xadd",
            "cmpxchg",
        ] {
            if op == "bswap" && w == 16 {
                continue;
            }
            let operands = match op {
                "bswap" => vec![Operand::Reg(0, w)],
                "shld" | "shrd" => vec![Operand::Reg(0, w), Operand::Reg(2, w), Operand::Reg(1, 8)],
                _ => vec![Operand::Reg(0, w), Operand::Reg(2, w)],
            };
            let address = p.instructions.len() as u64;
            p.instructions.push(Instruction {
                address,
                len: 1,
                op: op.into(),
                operands,
            });
            cases.push((address, w, op));
        }
    }
    for (op, operands) in [
        ("mov", vec![Operand::High(0), Operand::Reg(2, 8)]),
        ("xor", vec![Operand::High(0), Operand::High(0)]),
        ("add", vec![Operand::High(0), Operand::Reg(2, 8)]),
    ] {
        let address = p.instructions.len() as u64;
        p.instructions.push(Instruction {
            address,
            len: 1,
            op: op.into(),
            operands,
        });
        cases.push((address, 8, op));
    }
    let mut c = emit::c(&p).unwrap();
    c.push_str(
        r#"
#include <assert.h>
int main(void){uint64_t seed=71;
for(unsigned trial=0;trial<512;trial++){
seed=seed*UINT64_C(6364136223846793005)+1;uint64_t a=seed;
seed=seed*UINT64_C(6364136223846793005)+1;uint64_t b=seed;
if(trial<4){a=trial%2?0:UINT64_MAX;b=trial<2?a:~a;}
for(unsigned count=0;count<65;count++){
"#,
    );
    for (address, w, op) in cases {
        let suffix = match w {
            8 => "b",
            16 => "w",
            32 => "l",
            _ => "q",
        };
        let reg = match w {
            8 => "h",
            16 => "w",
            32 => "k",
            _ => "q",
        };
        writeln!(c,"{{rosemary_machine m={{0}};m.r[0]=a;m.r[2]=b;m.r[1]=count;m.rip={address};m.cf=m.zf=m.pf=1;m.sf=m.of=0;assert(rosemary_run(&m,1)==1);uint64_t left=a,right=b,flags;").unwrap();
        let native = match op {
            "bswap" => format!("bswap %{reg}0"),
            "shld" | "shrd" => format!("{op}{suffix} %%cl,%{reg}1,%{reg}0"),
            "xor" if w == 8 => "xorb %h0,%h0".into(),
            "mov" | "add" if w == 8 => format!("{op}b %b1,%h0"),
            _ => format!("{op}{suffix} %{reg}1,%{reg}0"),
        };
        writeln!(c,"__asm__ volatile(\"xorl %%r11d,%%r11d;stc;{native};pushfq;popq %2\":\"+a\"(left),\"+d\"(right),\"=r\"(flags):\"c\"((uint64_t)count):\"r11\",\"cc\",\"memory\");").unwrap();
        if matches!(op, "shld" | "shrd") {
            writeln!(c,"unsigned n=count&{};if(n<={w}){{assert(m.r[0]==left&&m.cf==(flags&1)&&m.zf==((flags>>6)&1)&&m.pf==((flags>>2)&1)&&m.sf==((flags>>7)&1));if(n<2)assert(m.of==((flags>>11)&1));}}",if w==64{63}else{31}).unwrap();
        } else if matches!(op, "bsf" | "bsr") {
            writeln!(
                c,
                "assert(m.zf==((flags>>6)&1));if(b&rosemary_vmask({w}))assert(m.r[0]==left);"
            )
            .unwrap();
        } else if matches!(op, "bt" | "bts" | "btr" | "btc") {
            c.push_str("assert(m.r[0]==left&&m.cf==(flags&1)&&m.zf==((flags>>6)&1));\n");
        } else {
            c.push_str("assert(m.r[0]==left&&m.r[2]==right&&m.cf==(flags&1)&&m.of==((flags>>11)&1)&&m.zf==((flags>>6)&1)&&m.sf==((flags>>7)&1)&&m.pf==((flags>>2)&1));\n");
        }
        c.push_str("}\n");
    }
    c.push_str("}}return 0;}\n");
    compile_run("bits-native", c);
}
#[test]
fn extended_encodings_and_traps_are_explicit() {
    let p=decode::decode(&input::hex("48 f7 e1 48 f7 e9 48 f7 f1 48 f7 f9 48 0f a3 c8 f3 0f bc c1 48 0f bd c1 48 0f a5 c8 48 0f c8 48 98 48 99 f0 48 0f c1 03 0f 0b cc").unwrap(),0,None).unwrap();
    assert!(p.instructions.iter().any(|i| i.op == "lock.xadd"));
    assert!(p.instructions.iter().any(|i| i.op == "ud2"));
    assert_eq!(Program::from_text(&p.to_text().unwrap()).unwrap(), p);
    assert!(Program::from_text("rosemary-ir 1\nentry 0\n0 1 lock.add r:64:0 i:64:1\n").is_err());
}

#[test]
fn count_instructions_match_native_when_the_host_has_the_required_features() {
    let mut p = Program {
        entry: 0,
        instructions: Vec::new(),
    };
    let mut cases = Vec::new();
    for (op, available) in [
        ("tzcnt", std::is_x86_feature_detected!("bmi1")),
        ("lzcnt", std::is_x86_feature_detected!("lzcnt")),
        ("popcnt", std::is_x86_feature_detected!("popcnt")),
    ] {
        if !available {
            continue;
        }
        for w in [16, 32, 64] {
            let address = p.instructions.len() as u64;
            p.instructions.push(Instruction {
                address,
                len: 1,
                op: op.into(),
                operands: vec![Operand::Reg(0, w), Operand::Reg(2, w)],
            });
            cases.push((address, w, op));
        }
    }
    if cases.is_empty() {
        return;
    }
    let mut c = emit::c(&p).unwrap();
    c.push_str("#include <assert.h>\nint main(void){uint64_t random=UINT64_C(0x123456789abcdef);for(unsigned bit=0;bit<=64;bit++)for(unsigned pattern=0;pattern<4;pattern++){random^=random<<13;random^=random>>7;random^=random<<17;uint64_t value=bit==64?0:UINT64_C(1)<<bit;if(pattern==1)value=~value;else if(pattern==2)value=random;else if(pattern==3)value=UINT64_MAX;\n");
    for (address, w, op) in cases {
        let reg = match w {
            16 => "w",
            32 => "k",
            _ => "q",
        };
        let suffix = match w {
            16 => "w",
            32 => "l",
            _ => "q",
        };
        writeln!(c,"{{rosemary_machine m={{0}};m.r[0]=UINT64_MAX;m.r[2]=value;m.rip={address};m.cf=m.of=m.sf=m.pf=m.zf=1;assert(rosemary_run(&m,1)==1);uint64_t expected=UINT64_MAX,flags;__asm__ volatile(\"{op}{suffix} %{reg}2,%{reg}0;pushfq;popq %1\":\"+a\"(expected),\"=r\"(flags):\"d\"(value):\"cc\",\"memory\");assert(m.r[0]==expected&&m.cf==(flags&1)&&m.zf==((flags>>6)&1));").unwrap();
        if op == "popcnt" {
            c.push_str(
                "assert(m.of==((flags>>11)&1)&&m.sf==((flags>>7)&1)&&m.pf==((flags>>2)&1));\n",
            );
        }
        c.push_str("}\n");
    }
    c.push_str("}return 0;}\n");
    compile_run("count-native", c);
}
