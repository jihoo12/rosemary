#![cfg(target_arch = "x86_64")]
use rosemary::{
    decode, emit, input,
    ir::{Instruction, Operand, Program, Segment},
};
use std::{fmt::Write, fs, process::Command};
fn run_c(name: &str, c: String) {
    let dir = std::env::temp_dir().join(format!("rosemary-{name}-{}", std::process::id()));
    fs::create_dir_all(&dir).unwrap();
    fs::write(dir.join("test.c"), c).unwrap();
    let output = Command::new("cc")
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
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(Command::new(dir.join("test")).status().unwrap().success());
    fs::remove_dir_all(dir).unwrap();
}
const MEMORY: &str = r#"
#include <assert.h>
#include <string.h>
static uint64_t load(void *ctx,uint64_t a,unsigned n){assert(a<=512-n);uint64_t v=0;memcpy(&v,(unsigned char*)ctx+a,n);return v;}
static void store(void *ctx,uint64_t a,unsigned n,uint64_t v){assert(a<=512-n);memcpy((unsigned char*)ctx+a,&v,n);}
"#;
#[test]
fn rep_movs_and_stos_match_native_in_both_directions_and_resume_with_a_budget() {
    let mut c = String::new();
    let mut cases = Vec::new();
    for (n, (op, w)) in ["movs", "stos"]
        .into_iter()
        .flat_map(|op| [8, 16, 32, 64].map(|w| (op, w)))
        .enumerate()
    {
        let p = Program {
            entry: 0,
            instructions: vec![Instruction {
                address: 0,
                len: 2,
                op: format!("rep.{op}"),
                operands: vec![Operand::Imm(w, 8)],
            }],
        };
        c.push_str(
            &emit::c(&p)
                .unwrap()
                .replace("rosemary_", &format!("rep_{n}_"))
                .replace("ROSEMARY_ENTRY", &format!("ENTRY_{n}")),
        );
        cases.push((n, op, w));
    }
    c.push_str(MEMORY);
    c.push_str("int main(void){\n");
    for (n, op, w) in cases {
        writeln!(c,"for(unsigned backwards=0;backwards<2;backwards++)for(unsigned count=0;count<=16;count++){{unsigned char memory[512],reference[512];for(unsigned j=0;j<512;j++)memory[j]=reference[j]=(unsigned char)(j*17+3);rep_{n}_machine m={{0}};m.ctx=memory;m.load=load;m.store=store;m.df=backwards;m.r[1]=count;m.r[6]=128+(backwards&&count?(count-1)*{}:0);m.r[7]=256+(backwards&&count?(count-1)*{}:0);m.r[0]=UINT64_C(0xfedcba9876543210);m.cf=m.of=m.zf=m.sf=m.pf=1;",w/8,w/8).unwrap();
        c.push_str("uint64_t remaining=count,value=m.r[0];unsigned char *source=reference+m.r[6],*destination=reference+m.r[7];\n");
        let suffix = match w {
            8 => "b",
            16 => "w",
            32 => "l",
            _ => "q",
        };
        let outputs = "+S";
        // Direction changes are restored before reentering ordinary host C.
        writeln!(c,"if(backwards){{__asm__ volatile(\"std;rep {op}{suffix};cld\":\"+D\"(destination),\"+c\"(remaining),\"{outputs}\"(source):\"a\"(value):\"cc\",\"memory\");}}else{{__asm__ volatile(\"cld;rep {op}{suffix}\":\"+D\"(destination),\"+c\"(remaining),\"{outputs}\"(source):\"a\"(value):\"cc\",\"memory\");}}").unwrap();
        c.push_str("unsigned visits=0;do{assert(rosemary_run_placeholder(&m,1)==1);visits++;assert(visits<=17);}while(m.rip==0);\n");
        let placeholder = "rosemary_run_placeholder";
        c = c.replace(placeholder, &format!("rep_{n}_run"));
        c.push_str("assert(m.r[1]==remaining&&m.r[7]==(uint64_t)(destination-reference)&&memcmp(memory,reference,512)==0);assert(m.cf&&m.of&&m.zf&&m.sf&&m.pf&&m.df==backwards);\n");
        if op == "movs" {
            c.push_str("assert(m.r[6]==(uint64_t)(source-reference));\n");
        }
        c.push_str("assert(visits==(count?count:1));}\n");
    }
    c.push_str("return 0;}\n");
    run_c("rep-native", c);
}
#[test]
fn fs_tls_and_gs_guest_bases_are_explicit_and_roundtrip() {
    let p = decode::decode(&input::hex("64 48 8b 03 65 48 89 03").unwrap(), 0, None).unwrap();
    assert!(matches!(
        p.instructions[0].operands[1],
        Operand::Mem {
            segment: Some(Segment::Fs),
            ..
        }
    ));
    assert_eq!(Program::from_text(&p.to_text().unwrap()).unwrap(), p);
    let mut c = emit::c(&p).unwrap();
    c.push_str(MEMORY);
    c.push_str(r#"
static _Thread_local uint64_t tls_value;
int main(void){unsigned char memory[512]={0};
 for(unsigned j=0;j<256;j++){
  tls_value=UINT64_C(0x1234567800000000)+j;
  uint64_t offset=(uint64_t)(uintptr_t)&tls_value-(uint64_t)(uintptr_t)__builtin_thread_pointer();
  uint64_t native;__asm__ volatile("movq %%fs:(%1),%0":"=r"(native):"r"(offset):"memory");
  rosemary_machine m={0};m.ctx=memory;m.load=load;m.store=store;m.fs_base=128-offset;m.r[3]=offset;store(memory,128,8,tls_value);
  assert(rosemary_run(&m,1)==1&&m.r[0]==native);
  m.gs_base=256-offset;assert(rosemary_run(&m,1)==1&&load(memory,256,8)==native);
 }return 0;
}
"#);
    run_c("segments", c);
    assert!(emit::assembly(&p).unwrap().contains("qword ptr fs:[rbx]"));
}
#[test]
fn locked_memory_updates_preserve_the_prefix_and_match_single_thread_native_execution() {
    let mut p = Program {
        entry: 0,
        instructions: Vec::new(),
    };
    let mut cases = Vec::new();
    for w in [8, 16, 32, 64] {
        for op in ["xadd", "cmpxchg"] {
            let address = p.instructions.len() as u64;
            p.instructions.push(Instruction {
                address,
                len: 1,
                op: format!("lock.{op}"),
                operands: vec![
                    Operand::Mem {
                        segment: None,
                        width: w,
                        base: Some(3),
                        index: None,
                        scale: 1,
                        disp: 0,
                    },
                    Operand::Reg(1, w),
                ],
            });
            cases.push((address, w, op));
        }
    }
    let mut c = emit::c(&p).unwrap();
    c.push_str(MEMORY);
    c.push_str(
        r#"
int main(void){uint64_t seed=89;for(unsigned trial=0;trial<256;trial++){
 seed=seed*UINT64_C(6364136223846793005)+1;uint64_t value=seed;
 seed=seed*UINT64_C(6364136223846793005)+1;uint64_t source=seed;
 uint64_t accumulator=trial%2?value:~value;
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
            8 => "b",
            16 => "w",
            32 => "k",
            _ => "q",
        };
        writeln!(c,"{{unsigned char memory[512]={{0}};store(memory,128,8,value);rosemary_machine m={{0}};m.ctx=memory;m.load=load;m.store=store;m.r[0]=accumulator;m.r[1]=source;m.r[3]=128;m.rip={address};assert(rosemary_run(&m,1)==1);uint64_t native_memory=value,left=accumulator,right=source,flags;__asm__ volatile(\"lock {op}{suffix} %{reg}1,%3;pushfq;popq %2\":\"+a\"(left),\"+r\"(right),\"=r\"(flags),\"+m\"(native_memory)::\"cc\",\"memory\");assert(m.r[0]==left&&m.r[1]==right&&load(memory,128,8)==native_memory);assert(m.cf==(flags&1)&&m.of==((flags>>11)&1)&&m.zf==((flags>>6)&1)&&m.sf==((flags>>7)&1)&&m.pf==((flags>>2)&1));}}").unwrap();
    }
    c.push_str("}return 0;}\n");
    run_c("lock-native", c);
    assert!(emit::assembly(&p).unwrap().contains("lock cmpxchg"));
}
#[test]
fn repeat_fixed_point_does_not_claim_initial_counter_on_reentry() {
    let p = Program::from_text(
        "rosemary-ir 1\nentry 0\n0 1 mov r:64:1 i:64:5\n1 1 cld\n2 2 rep.movs i:8:8\n4 1 ret\n",
    )
    .unwrap();
    let a = rosemary::analysis::analyze(&p).unwrap();
    assert_eq!(a.values.states[2].unwrap().registers[1], None);
    assert_eq!(a.values.states[3].unwrap().registers[1], Some(0));
    assert_eq!(a.values.states[3].unwrap().direction, Some(false));
    assert!(!a.loops.is_empty());
}

#[test]
fn memory_bit_offsets_use_signed_word_addressing_in_code_and_explanations() {
    let p=Program::from_text("rosemary-ir 1\nentry 0\n0 1 mov r:64:3 i:64:100\n1 1 mov r:64:1 i:64:ffffffffffffffff\n2 1 bt m:64:3:-:1:0 r:64:1\n3 1 ret\n").unwrap();
    let a = rosemary::dataflow::analyze(&p).unwrap();
    assert!(a.annotation(2).contains("effective address = 0xf8"));
    let mut c = emit::c(&p).unwrap();
    c.push_str(MEMORY);
    c.push_str(r#"
int main(void){unsigned char memory[512]={0};store(memory,248,8,UINT64_C(0x8000000000000000));
 rosemary_machine m={0};m.load=load;m.ctx=memory;assert(rosemary_run(&m,3)==1&&m.cf);
 unsigned carry;int64_t bit=-1;__asm__ volatile("btq %2,(%1);setc %b0":"=r"(carry):"r"(memory+256),"r"(bit):"cc","memory");
 assert(m.cf==(carry&255));return 0;}
"#);
    run_c("bit-memory", c);
}
