#![cfg(target_arch = "x86_64")]
use rosemary::{decode, emit, input};
use std::{fmt::Write, fs, path::Path, process::Command};

fn compile(source: &Path, output: &Path, extra: &[&str]) {
    let result = Command::new("cc")
        .args([
            "-std=c11",
            "-O2",
            "-Wall",
            "-Wextra",
            "-Werror",
            "-mno-red-zone",
        ])
        .args(extra)
        .arg(source)
        .arg("-o")
        .arg(output)
        .output()
        .unwrap();
    assert!(
        result.status.success(),
        "{}",
        String::from_utf8_lossy(&result.stderr)
    );
}

#[test]
fn generated_arithmetic_matches_native_cpu_at_boundaries_and_random_inputs() {
    let cases = [
        ("48 0f af c1", "imulq", 64, "imul"),
        ("0f af c1", "imull", 32, "imul"),
        ("48 6b c1 fd", "imulq $-3, %2, %0", 64, "imul3"),
        ("69 c1 fb ff ff ff", "imull $-5, %2, %0", 32, "imul3"),
        ("48 ff c0", "incq", 64, "unary"),
        ("ff c0", "incl", 32, "unary"),
        ("48 ff c8", "decq", 64, "unary"),
        ("ff c8", "decl", 32, "unary"),
        ("48 f7 d8", "negq", 64, "unary"),
        ("f7 d8", "negl", 32, "unary"),
        ("48 f7 d0", "notq", 64, "not"),
        ("f7 d0", "notl", 32, "not"),
        ("48 63 c1", "", 64, "movsxd"),
        ("f3 0f 1e fa", "", 64, "endbr"),
    ];
    let bytes = input::hex(&cases.iter().map(|c| c.0).collect::<Vec<_>>().join(" ")).unwrap();
    let p = decode::decode(&bytes, 0x1000, None).unwrap();
    let mut c = emit::c(&p).unwrap();
    c.push_str(
        r#"
#include <assert.h>
int main(void) {
    uint64_t values[48] = {0,1,2,3,0x7fffffff,0x80000000,0xffffffff,
        UINT64_C(0x7fffffffffffffff),UINT64_C(0x8000000000000000),UINT64_MAX,UINT64_MAX-1};
    uint64_t seed=7;
    for(unsigned n=11;n<48;n++) { seed=seed*UINT64_C(6364136223846793005)+1; values[n]=seed; }
    for(unsigned j=0;j<48;j++) for(unsigned k=0;k<48;k++) {
        uint64_t a=values[j], b=values[k];
"#,
    );
    for ((_, native, width, kind), instruction) in cases.iter().zip(&p.instructions) {
        writeln!(c, "{{ rosemary_machine m={{0}}; m.r[0]=a; m.r[1]=b; m.cf=m.of=m.zf=m.sf=m.pf=1; m.rip=UINT64_C(0x{:x});", instruction.address).unwrap();
        c.push_str("assert(rosemary_run(&m,1)==1);\n");
        if *kind == "movsxd" {
            c.push_str("assert(m.r[0]==(((b & UINT64_C(0xffffffff)) ^ UINT64_C(0x80000000))-UINT64_C(0x80000000))); assert(m.cf && m.of && m.zf && m.sf && m.pf);\n");
        } else if *kind == "endbr" {
            c.push_str("assert(m.r[0]==a && m.r[1]==b && m.cf && m.of && m.zf && m.sf && m.pf);\n");
        } else {
            writeln!(
                c,
                "uint{width}_t expected=(uint{width}_t)a, rhs=(uint{width}_t)b; uint64_t flags;"
            )
            .unwrap();
            let operation = match *kind {
                "imul" => format!("{native} %2, %0"),
                "imul3" => native.to_string(),
                _ => format!("{native} %0"),
            };
            writeln!(c, "__asm__ volatile(\"stc; {operation}; pushfq; popq %1\" : \"+r\"(expected), \"=r\"(flags) : \"r\"(rhs) : \"cc\", \"memory\");").unwrap();
            c.push_str("assert(m.r[0]==expected);\n");
            if *kind == "not" {
                c.push_str("assert(m.cf && m.of && m.zf && m.sf && m.pf); (void)flags;\n");
            } else {
                c.push_str("assert(m.cf==(flags&1) && m.of==((flags>>11)&1));\n");
                if *kind == "unary" {
                    c.push_str("assert(m.pf==((flags>>2)&1) && m.zf==((flags>>6)&1) && m.sf==((flags>>7)&1));\n");
                }
            }
        }
        c.push_str("}\n");
    }
    c.push_str("} return 0; }\n");
    let dir = std::env::temp_dir().join(format!("rosemary-native-{}", std::process::id()));
    fs::create_dir_all(&dir).unwrap();
    fs::write(dir.join("native.c"), c).unwrap();
    compile(&dir.join("native.c"), &dir.join("native"), &[]);
    assert!(Command::new(dir.join("native")).status().unwrap().success());
    fs::write(dir.join("native.s"), emit::assembly(&p).unwrap()).unwrap();
    compile(&dir.join("native.s"), &dir.join("native.o"), &["-c"]);
    fs::remove_dir_all(dir).unwrap();
}

#[test]
fn real_compiler_elf_to_recursive_ir_to_executable_c() {
    let dir = std::env::temp_dir().join(format!("rosemary-elf-execution-{}", std::process::id()));
    fs::create_dir_all(&dir).unwrap();
    let source = "unsigned long factorial(unsigned long n) { unsigned long r=1; while(n>1) r*=n--; return r; }\nunsigned long decoy(unsigned long n) { return n+100; }\n";
    fs::write(dir.join("factorial.c"), source).unwrap();
    compile(
        &dir.join("factorial.c"),
        &dir.join("factorial.elf"),
        &[
            "-O1",
            "-nostdlib",
            "-no-pie",
            "-fcf-protection=branch",
            "-Wl,-e,decoy",
        ],
    );
    let bin = env!("CARGO_BIN_EXE_rosemary");
    let lifted = Command::new(bin)
        .arg("lift")
        .arg(dir.join("factorial.elf"))
        .args(["--function", "factorial"])
        .output()
        .unwrap();
    assert!(
        lifted.status.success(),
        "{}",
        String::from_utf8_lossy(&lifted.stderr)
    );
    let p = rosemary::ir::Program::from_text(std::str::from_utf8(&lifted.stdout).unwrap()).unwrap();
    let bytes = fs::read(dir.join("factorial.elf")).unwrap();
    let symbol = input::elf_functions(&bytes)
        .unwrap()
        .into_iter()
        .find(|s| s.name == "factorial")
        .unwrap();
    assert_eq!(p.entry, symbol.address);
    assert_ne!(p.entry, input::elf_text(&bytes).unwrap().entry);
    assert!(
        p.instructions.iter().all(|i| i.address >= symbol.address
            && i.address + i.len as u64 <= symbol.address + symbol.size)
    );
    for to in ["text", "json"] {
        let output = Command::new(bin)
            .arg("symbols")
            .arg(dir.join("factorial.elf"))
            .args(["--to", to])
            .output()
            .unwrap();
        assert!(output.status.success());
        assert!(
            String::from_utf8(output.stdout)
                .unwrap()
                .contains("factorial")
        );
    }
    let a = rosemary::analysis::analyze(&p).unwrap();
    assert_eq!(a.loops.len(), 1);
    let mut c = emit::c(&p).unwrap();
    c.push_str(source);
    c.push_str(
        r#"
#include <assert.h>
static uint64_t load(void *ctx, uint64_t address, unsigned width) {
    (void)ctx; assert(address==512 && width==8); return UINT64_C(0xdead);
}
int main(void) {
    for(unsigned n=0;n<=25;n++) {
        rosemary_machine m={0}; m.rip=ROSEMARY_ENTRY; m.load=load; m.r[7]=n; m.r[4]=512;
        assert(rosemary_run(&m,1000)==0);
        assert(m.r[0]==factorial(n) && m.rip==UINT64_C(0xdead) && m.r[4]==520);
    }
    return 0;
}
"#,
    );
    fs::write(dir.join("translated.c"), c).unwrap();
    compile(&dir.join("translated.c"), &dir.join("translated"), &[]);
    assert!(
        Command::new(dir.join("translated"))
            .status()
            .unwrap()
            .success()
    );
    fs::remove_dir_all(dir).unwrap();
}

#[test]
fn inferred_facts_hold_during_execution_and_simplified_ir_is_equivalent() {
    use rosemary::{dataflow, ir::Program};
    let programs = [
        "rosemary-ir 1\nentry 0\n0 1 xor r:32:0 r:32:0\n1 1 jne i:64:5\n2 1 mov r:32:0 i:32:6\n3 1 imul r:32:0 r:32:0 i:32:7\n4 1 ret\n5 1 mov r:32:0 i:32:ff\n6 1 ret\n",
        include_str!("../examples/sum.ir"),
        "rosemary-ir 1\nentry 0\n0 1 test r:64:7 r:64:7\n1 1 je i:64:5\n2 1 mov r:32:0 i:32:2a\n3 1 jmp i:64:7\n5 1 mov r:32:0 i:32:2b\n6 1 nop\n7 1 ret\n",
        "rosemary-ir 1\nentry 0\n0 1 mov r:64:3 i:64:4\n1 1 jmp r:64:3\n2 1 mov r:64:0 i:64:ff\n3 1 ret\n4 1 xor r:32:0 r:32:0\n5 1 ret\n",
        include_str!("../examples/recovery.ir"),
        "rosemary-ir 1\nentry 0\n0 1 mov r:64:3 i:64:80\n1 1 mov m:64:3:-:1:0 r:64:0\n2 1 mov r:64:1 m:64:3:-:1:0\n3 1 cmp r:64:0 r:64:1\n4 1 jne i:64:6\n5 1 inc r:64:0\n6 1 ret\n",
        "rosemary-ir 1\nentry 0\n0 1 mov r:64:0 i:64:ffffffffffffffff\n1 1 mov r:16:0 i:16:1234\n2 1 xor r:8:0 r:8:0\n3 1 shl r:32:0 i:8:1\n4 1 sete r:8:2\n5 1 cmovne r:64:0 r:64:7\n6 1 ret\n",
    ];
    let mut c = String::from("#include <assert.h>\n#include <string.h>\n");
    let parsed: Vec<_> = programs
        .iter()
        .map(|s| Program::from_text(s).unwrap())
        .collect();
    for (n, p) in parsed.iter().enumerate() {
        let a = dataflow::analyze(p).unwrap();
        for (name, program) in [
            ("original", p.clone()),
            ("simplified", a.simplify().unwrap()),
        ] {
            c.push_str(
                &emit::c(&program)
                    .unwrap()
                    .replace("rosemary_", &format!("{name}_{n}_"))
                    .replace("ROSEMARY_ENTRY", &format!("ENTRY_{name}_{n}")),
            );
        }
    }
    c.push_str(
        r#"
static uint64_t load(void *ctx,uint64_t address,unsigned n) {
    assert(address<=1024-n); unsigned char *bytes=ctx; uint64_t v=0;
    for(unsigned i=0;i<n;i++) { v|=(uint64_t)bytes[address+i]<<(8*i); }
    return v;
}
static void store(void *ctx,uint64_t address,unsigned n,uint64_t v) {
    assert(address<=1024-n); unsigned char *bytes=ctx;
    for(unsigned i=0;i<n;i++) bytes[address+i]=(unsigned char)(v>>(8*i));
}
int main(void) {
    uint64_t seed=19;
"#,
    );
    for (n, p) in parsed.iter().enumerate() {
        let a = dataflow::analyze(p).unwrap();
        writeln!(c,"for(unsigned trial=0;trial<256;trial++) {{ original_{n}_machine m={{0}}; simplified_{n}_machine s={{0}}; unsigned char mem[1024]={{0}}, other[1024]={{0}};").unwrap();
        c.push_str("for(unsigned r=0;r<16;r++) { seed=seed*UINT64_C(6364136223846793005)+1; m.r[r]=s.r[r]=seed; }\n");
        c.push_str("m.r[7]=s.r[7]=trial%2; m.r[4]=s.r[4]=512; store(mem,512,8,0xdead); store(other,512,8,0xdead); m.ctx=mem; s.ctx=other; m.load=s.load=load; m.store=s.store=store; m.cf=s.cf=trial%2; m.zf=s.zf=(trial/2)%2; m.sf=s.sf=(trial/4)%2; m.of=s.of=(trial/8)%2; m.pf=s.pf=(trial/16)%2;\n");
        writeln!(c,"m.rip=s.rip=UINT64_C(0x{:x});\nfor(unsigned step=0;step<200;step++) {{ switch(m.rip) {{",p.entry).unwrap();
        for (index, i) in p.instructions.iter().enumerate() {
            writeln!(c, "case UINT64_C(0x{:x}):", i.address).unwrap();
            if let Some(state) = a.states[index] {
                for (reg, value) in state.registers.iter().enumerate() {
                    if let Some(value) = value {
                        writeln!(c, "assert(m.r[{reg}]==UINT64_C(0x{value:x}));").unwrap();
                    }
                }
                for (name, value) in [
                    ("zf", state.flags.zf),
                    ("sf", state.flags.sf),
                    ("cf", state.flags.cf),
                    ("of", state.flags.of),
                    ("pf", state.flags.pf),
                ] {
                    if let Some(value) = value {
                        writeln!(c, "assert(m.{name}=={});", u8::from(value)).unwrap();
                    }
                }
            } else {
                c.push_str("assert(0 && \"value analysis lost a feasible path\");\n");
            }
            c.push_str("break;\n");
        }
        writeln!(
            c,
            "default: break; }} int left=original_{n}_run(&m,1), right=simplified_{n}_run(&s,1);"
        )
        .unwrap();
        c.push_str("assert(left==right && m.rip==s.rip && memcmp(m.r,s.r,sizeof(m.r))==0 && memcmp(mem,other,sizeof(mem))==0); assert(m.cf==s.cf && m.of==s.of && m.zf==s.zf && m.sf==s.sf && m.pf==s.pf); if(!left) break; assert(step<199); } }\n");
    }
    c.push_str("return 0; }\n");
    let dir = std::env::temp_dir().join(format!(
        "rosemary-dataflow-execution-{}",
        std::process::id()
    ));
    fs::create_dir_all(&dir).unwrap();
    fs::write(dir.join("equivalence.c"), c).unwrap();
    compile(&dir.join("equivalence.c"), &dir.join("equivalence"), &[]);
    assert!(
        Command::new(dir.join("equivalence"))
            .status()
            .unwrap()
            .success()
    );
    fs::remove_dir_all(dir).unwrap();
}
