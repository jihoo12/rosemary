#![cfg(target_arch = "x86_64")]
use rosemary::{
    decode, emit,
    ir::{Instruction, Operand, Program},
};
use std::{fmt::Write, fs, process::Command};

fn compile_run(name: &str, source: &str) {
    let dir = std::env::temp_dir().join(format!("rosemary-{name}-{}", std::process::id()));
    fs::create_dir_all(&dir).unwrap();
    fs::write(dir.join("test.c"), source).unwrap();
    let output = Command::new("cc")
        .args(["-std=c11", "-O2", "-Wall", "-Wextra", "-Werror"])
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

#[test]
fn memory_compare_exchange_writes_once_on_both_success_and_failure() {
    let mut instructions = Vec::new();
    for width in [8, 16, 32, 64] {
        for locked in [false, true] {
            instructions.push(Instruction {
                address: instructions.len() as u64,
                len: 1,
                op: if locked { "lock.cmpxchg" } else { "cmpxchg" }.into(),
                operands: vec![
                    Operand::Mem {
                        segment: None,
                        width,
                        base: Some(3),
                        index: None,
                        scale: 1,
                        disp: 0,
                    },
                    Operand::Reg(1, width),
                ],
            });
        }
    }
    let p = Program {
        entry: 0,
        instructions,
    };
    let mut c = emit::c(&p).unwrap();
    c.push_str(
        r#"
#include <assert.h>
#include <string.h>
typedef struct { uint64_t value,mask;unsigned width,events; } memory;
static uint64_t load(void *ctx,uint64_t address,unsigned width) {
 memory *mem=ctx;assert(address==128&&width==mem->width&&mem->events++==0);
 return mem->value&mem->mask;
}
static void store(void *ctx,uint64_t address,unsigned width,uint64_t value) {
 memory *mem=ctx;assert(address==128&&width==mem->width&&mem->events++==1);
 mem->value=(mem->value&~mem->mask)|(value&mem->mask);
}
int main(void) {
 uint64_t seed=91;
 for(unsigned trial=0;trial<512;trial++) {
  seed=seed*UINT64_C(6364136223846793005)+1;uint64_t value=seed;
  seed=seed*UINT64_C(6364136223846793005)+1;uint64_t source=seed;
  uint64_t accumulator=trial&1?value:~value;
"#,
    );
    for i in &p.instructions {
        let width = i.operands[0].width();
        let mask = if width == 64 {
            u64::MAX
        } else {
            (1u64 << width) - 1
        };
        let suffix = match width {
            8 => "b",
            16 => "w",
            32 => "l",
            _ => "q",
        };
        let reg = match width {
            8 => "b",
            16 => "w",
            32 => "k",
            _ => "q",
        };
        let prefix = if i.op.starts_with("lock.") {
            "lock "
        } else {
            ""
        };
        writeln!(c, "{{memory mem={{value,UINT64_C(0x{mask:x}),{},0}};rosemary_machine m={{0}};m.ctx=&mem;m.load=load;m.store=store;m.r[0]=accumulator;m.r[1]=source;m.r[3]=128;m.rip={};m.cf=m.zf=m.sf=m.of=m.pf=m.df=trial&1;rosemary_machine initial=m;assert(rosemary_run(&m,0)==1&&memcmp(&m,&initial,sizeof(m))==0&&mem.events==0);assert(rosemary_run(&m,1)==1&&mem.events==2);uint64_t native=value,left=accumulator,right=source,flags;__asm__ volatile(\"{prefix}cmpxchg{suffix} %{reg}1,%3;pushfq;popq %2\":\"+a\"(left),\"+c\"(right),\"=r\"(flags),\"+m\"(native)::\"cc\",\"memory\");assert(m.r[0]==left&&m.r[1]==right&&mem.value==native&&m.r[3]==128);assert(m.cf==(flags&1)&&m.pf==((flags>>2)&1)&&m.zf==((flags>>6)&1)&&m.sf==((flags>>7)&1)&&m.of==((flags>>11)&1)&&m.df==(trial&1));assert(m.rip=={});}}",width/8,i.address,i.address+1).unwrap();
    }
    c.push_str("}return 0;}\n");
    compile_run("cmpxchg-memory", &c);
}

#[test]
fn failed_memory_compare_exchange_uses_address_before_accumulator_update() {
    // FS:[RAX] on failure changes AL/AX/EAX/RAX, but not the pending store EA.
    for (width, prefixes, opcode) in [
        (8, vec![], 0xb0),
        (16, vec![0x66], 0xb1),
        (32, vec![], 0xb1),
        (64, vec![0x48], 0xb1),
    ] {
        let mut bytes = vec![0x64, 0xf0];
        bytes.extend(prefixes);
        bytes.extend([0x0f, opcode, 0x08]);
        let p = decode::decode(&bytes, 0x1000, None).unwrap();
        let mut c = emit::c(&p).unwrap();
        c.push_str(r#"
#include <assert.h>
typedef struct {unsigned events;} memory;
static uint64_t load(void *ctx,uint64_t address,unsigned width){memory *mem=ctx;assert(mem->events++==0&&address==384&&width==WIDTH);return 7;}
static void store(void *ctx,uint64_t address,unsigned width,uint64_t value){memory *mem=ctx;assert(mem->events++==1&&address==384&&width==WIDTH&&value==7);}
int main(void){memory mem={0};rosemary_machine m={0};m.ctx=&mem;m.load=load;m.store=store;m.r[0]=128;m.r[1]=42;m.fs_base=256;m.rip=0x1000;assert(rosemary_run(&m,1)==1&&mem.events==2&&m.r[0]==7&&m.r[1]==42&&m.fs_base==256&&!m.zf);return 0;}
"#);
        c = c.replace("WIDTH", &(width / 8).to_string());
        compile_run(&format!("cmpxchg-address-{width}"), &c);
    }
}

#[test]
fn register_compare_exchange_preserves_failed_destination_and_handles_aliases() {
    let mut instructions = Vec::new();
    for width in [8, 16, 32, 64] {
        for (dst, src) in [(2, 1), (2, 2), (0, 1), (2, 0)] {
            instructions.push(Instruction {
                address: instructions.len() as u64,
                len: 1,
                op: "cmpxchg".into(),
                operands: vec![Operand::Reg(dst, width), Operand::Reg(src, width)],
            });
        }
    }
    instructions.push(Instruction {
        address: instructions.len() as u64,
        len: 1,
        op: "cmpxchg".into(),
        operands: vec![Operand::High(0), Operand::Reg(1, 8)],
    });
    let p = Program {
        entry: 0,
        instructions,
    };
    let mut c = emit::c(&p).unwrap();
    c.push_str(
        r#"
#include <assert.h>
int main(void){uint64_t seed=17;for(unsigned trial=0;trial<512;trial++){
 seed=seed*UINT64_C(6364136223846793005)+1;uint64_t a=seed;
 seed=seed*UINT64_C(6364136223846793005)+1;uint64_t b=seed;
 seed=seed*UINT64_C(6364136223846793005)+1;uint64_t d=seed;
 if(trial&1)d=(d&UINT64_C(0xffffffff00000000))|(uint32_t)a;
"#,
    );
    for i in &p.instructions {
        let width = i.operands[0].width();
        let suffix = match width {
            8 => "b",
            16 => "w",
            32 => "l",
            _ => "q",
        };
        let reg = match width {
            8 => "b",
            16 => "w",
            32 => "k",
            _ => "q",
        };
        let native_operand = |o: &Operand| match o {
            Operand::Reg(r, _) => format!(
                "%{reg}{}",
                match r {
                    0 => 0,
                    1 => 1,
                    _ => 2,
                }
            ),
            Operand::High(0) => "%h0".into(),
            _ => unreachable!(),
        };
        writeln!(c,"{{rosemary_machine m={{0}};m.r[0]=a;m.r[1]=b;m.r[2]=d;m.rip={};assert(rosemary_run(&m,1)==1);uint64_t left=a,source=b,destination=d,flags;__asm__ volatile(\"cmpxchg{suffix} {},{};pushfq;popq %3\":\"+a\"(left),\"+c\"(source),\"+d\"(destination),\"=r\"(flags)::\"cc\",\"memory\");assert(m.r[0]==left&&m.r[1]==source&&m.r[2]==destination);assert(m.cf==(flags&1)&&m.pf==((flags>>2)&1)&&m.zf==((flags>>6)&1)&&m.sf==((flags>>7)&1)&&m.of==((flags>>11)&1));}}",i.address,native_operand(&i.operands[1]),native_operand(&i.operands[0])).unwrap();
    }
    c.push_str("}return 0;}");
    compile_run("cmpxchg-registers", &c);
}
