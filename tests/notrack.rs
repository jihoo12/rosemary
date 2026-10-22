use rosemary::{
    analysis::{self, EdgeKind},
    dataflow,
    decode::{self, RecursiveOptions},
    emit, input,
    ir::{Operand, Program, Segment},
};
#[cfg(target_arch = "x86_64")]
use std::{fmt::Write, fs, process::Command};

#[test]
fn notrack_preserves_prefixes_and_suppresses_them_when_fs_or_gs_is_present() {
    for (encoding, op, falls_through) in [
        ("3e ff e0", "notrack.jmp", false),
        ("3e 41 ff e0", "notrack.jmp", false),
        ("3e ff d0", "notrack.call", true),
        ("3e ff 10", "notrack.call", true),
        ("3e ff 25 10 00 00 00", "notrack.jmp", false),
        ("3e 3e ff e0", "notrack.jmp", false),
        ("3e 2e ff e0", "notrack.jmp", false),
        ("64 3e ff e0", "jmp", false),
        ("64 3e ff 20", "jmp", false),
        ("3e 65 ff 10", "call", true),
    ] {
        let bytes = input::hex(encoding).unwrap();
        let p = decode::decode(&bytes, 0x1000, None).unwrap();
        let i = &p.instructions[0];
        assert_eq!(i.op, op);
        assert_eq!(i.len as usize, bytes.len());
        assert!(i.is_control_flow());
        assert_eq!(i.falls_through(), falls_through);
        assert_eq!(Program::from_text(&p.to_text().unwrap()).unwrap(), p);
        if op.starts_with("notrack.") {
            assert!(emit::assembly(&p).unwrap().contains("notrack "));
        }
    }
    let p = decode::decode(&input::hex("64 3e ff 20").unwrap(), 0, None).unwrap();
    assert!(matches!(
        p.instructions[0].operands[0],
        Operand::Mem {
            segment: Some(Segment::Fs),
            ..
        }
    ));
    let p = decode::decode_recursive(
        &input::hex("3e ff e0 0f 05").unwrap(),
        0,
        0,
        RecursiveOptions::default(),
    )
    .unwrap();
    assert_eq!(p.instructions.len(), 1);
    let p = decode::decode_recursive(
        &input::hex("3e ff d0 c3 0f 05").unwrap(),
        0,
        0,
        RecursiveOptions::default(),
    )
    .unwrap();
    assert_eq!(p.instructions.len(), 2);
}

#[test]
fn invalid_notrack_forms_and_truncated_prefixes_are_rejected() {
    for encoding in [
        "3e",
        "3e ff",
        "3e ff 25 00",
        "3e 90",
        "3e c3",
        "3e e9 00 00 00 00",
        "3e 48 8b 00",
        "f0 3e ff 10",
        "3e f3 ff e0",
    ] {
        assert!(
            decode::decode(&input::hex(encoding).unwrap(), 0, None).is_err(),
            "{encoding}"
        );
    }
    assert!(decode::decode(&[0x3e; 16], 0, None).is_err());
    for line in [
        "notrack.jmp i:64:0",
        "notrack.call r:32:0",
        "notrack.jne i:64:0",
        "notrack.ret",
        "notrack.lock.jmp r:64:0",
        "lock.notrack.call m:64:0:-:1:0",
        "notrack.jmp m:64:0:-:1:0:fs",
    ] {
        assert!(
            Program::from_text(&format!("rosemary-ir 1\nentry 0\n0 1 {line}\n")).is_err(),
            "{line}"
        );
    }
}

#[test]
fn notrack_control_flow_infers_targets_and_simplifies_to_plain_direct_transfers() {
    for op in ["jmp", "call"] {
        let p = Program::from_text(&format!("rosemary-ir 1\nentry 0\n0 1 mov r:64:0 i:64:4\n1 1 notrack.{op} r:64:0\n2 1 ret\n4 1 ret\n")).unwrap();
        let a = analysis::analyze(&p).unwrap();
        assert!(!a.values.conservative_indirect);
        assert_eq!(a.values.target(1), Some(4));
        let kind = if op == "call" {
            EdgeKind::Call
        } else {
            EdgeKind::Jump
        };
        assert!(
            a.edges
                .iter()
                .any(|e| e.kind == kind && e.target == Some(4) && e.inferred)
        );
        if op == "call" {
            assert_eq!(a.function_entries, [0, 4]);
            assert_eq!(a.values.states[2].unwrap().registers[0], None);
            assert!(a.values.annotation(1).contains("call barrier"));
        } else {
            assert!(a.values.states[2].is_none());
        }
        let simplified = a.values.simplify().unwrap();
        assert_eq!(simplified.instructions[1].op, op);
        assert_eq!(simplified.instructions[1].operands, [Operand::Imm(4, 64)]);
    }
    let p =
        Program::from_text("rosemary-ir 1\nentry 0\n0 1 notrack.jmp r:64:0\n1 1 ret\n").unwrap();
    let a = dataflow::analyze(&p).unwrap();
    assert!(a.conservative_indirect);
    assert_eq!(a.simplify().unwrap(), p);
}

#[test]
#[cfg(target_arch = "x86_64")]
fn notrack_c_calls_match_simplification_and_native_assembly_preserves_dispatch() {
    let p = decode::decode(
        &input::hex("48 b8 10 10 00 00 00 00 00 00 3e ff d0 c3 cc cc b8 2a 00 00 00 c3").unwrap(),
        0x1000,
        None,
    )
    .unwrap();
    let simplified = dataflow::analyze(&p).unwrap().simplify().unwrap();
    let mut c = String::new();
    for (name, program) in [("original", p), ("direct", simplified)] {
        c.push_str(
            &emit::c(&program)
                .unwrap()
                .replace("rosemary_", &format!("{name}_"))
                .replace("ROSEMARY_ENTRY", &format!("ENTRY_{name}")),
        );
    }
    for (name, bytes) in [("memory", "3e ff 14 24"), ("segmented", "64 3e ff 14 24")] {
        let p = decode::decode(&input::hex(bytes).unwrap(), 0x2000, None).unwrap();
        c.push_str(
            &emit::c(&p)
                .unwrap()
                .replace("rosemary_", &format!("{name}_"))
                .replace("ROSEMARY_ENTRY", &format!("ENTRY_{name}")),
        );
    }
    c.push_str(r#"
#include <assert.h>
#include <string.h>
typedef struct { uint64_t words[128]; unsigned loads,stores; uint64_t last_load,last_store; } memory;
static uint64_t load(void *ctx,uint64_t a,unsigned n){memory *mem=ctx;assert(a<1024&&a%8==0&&n==8);mem->loads++;mem->last_load=a;return mem->words[a/8];}
static void store(void *ctx,uint64_t a,unsigned n,uint64_t v){memory *mem=ctx;assert(a<1024&&a%8==0&&n==8);mem->stores++;mem->last_store=a;mem->words[a/8]=v;}
uint64_t native_target(void){return 42;}
extern uint64_t native_jump_entry(uint64_t);
extern uint64_t native_call_entry(uint64_t);
int main(void){
 original_machine m={0};direct_machine s={0};memory mem={0},other={0};
 mem.words[64]=other.words[64]=0xdead;m.ctx=&mem;s.ctx=&other;
 m.load=s.load=load;m.store=s.store=store;m.r[4]=s.r[4]=512;m.rip=s.rip=0x1000;
 m.cf=m.zf=m.sf=m.of=m.pf=m.df=s.cf=s.zf=s.sf=s.of=s.pf=s.df=1;
 for(unsigned step=0;step<5;step++){
  assert(original_run(&m,1)==1&&direct_run(&s,1)==1);
  assert(m.rip==s.rip&&memcmp(m.r,s.r,sizeof(m.r))==0&&memcmp(mem.words,other.words,sizeof(mem.words))==0);
  assert(m.cf==s.cf&&m.zf==s.zf&&m.sf==s.sf&&m.of==s.of&&m.pf==s.pf&&m.df==s.df);
 }
 assert(m.r[0]==42&&m.r[4]==520&&m.rip==0xdead&&mem.words[63]==0x100d);
 assert(mem.loads==2&&mem.stores==1&&other.loads==2&&other.stores==1);
 assert(original_run(&m,1)==0&&direct_run(&s,1)==0);
 assert(native_jump_entry((uint64_t)(uintptr_t)native_target)==42);
 assert(native_call_entry((uint64_t)(uintptr_t)native_target)==42);
"#);
    for (name, address, next) in [("memory", 128, 0x2004), ("segmented", 384, 0x2005)] {
        writeln!(c, "{{{name}_machine m={{0}};memory mem={{0}};mem.words[{}]=0xdeadbeef;m.ctx=&mem;m.load=load;m.store=store;m.r[4]=128;m.fs_base=256;m.rip=0x2000;m.cf=m.zf=m.sf=m.of=m.pf=m.df=1;assert({name}_run(&m,1)==1);assert(m.rip==0xdeadbeef&&m.r[4]==120&&mem.loads==1&&mem.last_load=={address}&&mem.stores==1&&mem.last_store==120&&mem.words[15]=={next});assert(m.cf&&m.zf&&m.sf&&m.of&&m.pf&&m.df);}}",address/8).unwrap();
    }
    c.push_str("return 0;}\n");
    let jump = Program::from_text("rosemary-ir 1\nentry 0\n0 3 notrack.jmp r:64:7\n").unwrap();
    let call = Program::from_text("rosemary-ir 1\nentry 0\n0 1 sub r:64:4 i:64:8\n1 1 notrack.call r:64:7\n2 1 add r:64:4 i:64:8\n3 1 ret\n").unwrap();
    let mut assembly = emit::assembly(&jump)
        .unwrap()
        .replace("rosemary_entry", "native_jump_entry");
    assembly.push_str(
        &emit::assembly(&call)
            .unwrap()
            .replace("rosemary_entry", "native_call_entry")
            .replace(".L_", ".L_call_"),
    );
    assembly.push_str(".section .note.GNU-stack,\"\",@progbits\n");
    let dir = std::env::temp_dir().join(format!("rosemary-notrack-{}", std::process::id()));
    fs::create_dir_all(&dir).unwrap();
    fs::write(dir.join("test.c"), c).unwrap();
    fs::write(dir.join("native.s"), assembly).unwrap();
    let output = Command::new("cc")
        .args(["-std=c11", "-O2", "-Wall", "-Wextra", "-Werror"])
        .arg(dir.join("test.c"))
        .arg(dir.join("native.s"))
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
