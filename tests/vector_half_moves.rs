use rosemary::{
    decode, emit, input,
    ir::{Operand, Program},
};
#[cfg(target_arch = "x86_64")]
use std::{fmt::Write, fs, process::Command};

#[test]
fn half_vector_memory_forms_are_distinguished_from_register_shuffles() {
    for (prefix, low, high) in [("", "movlps", "movhps"), ("66", "movlpd", "movhpd")] {
        for (opcode, store, op) in [
            ("12", false, low),
            ("13", true, low),
            ("16", false, high),
            ("17", true, high),
        ] {
            let p = decode::decode(
                &input::hex(&format!("{prefix} 44 0f {opcode} 43 f0")).unwrap(),
                0x1000,
                None,
            )
            .unwrap();
            assert_eq!(p.instructions[0].op, op);
            let a = &p.instructions[0].operands;
            assert_eq!(a[usize::from(store)], Operand::Vec(8));
            assert!(
                matches!(a[usize::from(!store)], Operand::Mem {width:64,base:Some(3),disp,..} if disp==(-16i64) as u64)
            );
            assert_eq!(Program::from_text(&p.to_text().unwrap()).unwrap(), p);
            assert!(emit::assembly(&p).unwrap().contains("qword ptr"));
        }
    }
    let p = decode::decode(&input::hex("0f 12 c1 0f 16 c1").unwrap(), 0, None).unwrap();
    assert_eq!(p.instructions[0].op, "movhlps");
    assert_eq!(p.instructions[1].op, "movlhps");
    let p = decode::decode(&input::hex("0f 16 85 68 ff ff ff").unwrap(), 0xd12f7, None).unwrap();
    assert_eq!(p.instructions[0].op, "movhps");
    assert_eq!(p.instructions[0].operands[1].width(), 64);
    for encoding in ["0f 13 c1", "0f 17 c1", "66 0f 12 c1", "66 0f 16 c1"] {
        let error = decode::decode(&input::hex(encoding).unwrap(), 0x1000, None).unwrap_err();
        assert!(
            error.contains("0x1000: invalid partial vector move encoding"),
            "{error}"
        );
    }
    for operands in [
        "x:128:0 x:128:1",
        "x:128:0 m:128:0:-:1:0",
        "r:64:0 x:128:0",
        "m:32:0:-:1:0 x:128:0",
    ] {
        assert!(
            Program::from_text(&format!("rosemary-ir 1\nentry 0\n0 1 movhps {operands}\n"))
                .is_err()
        );
    }
    let bytes = input::hex("66 44 0f 17 84 24 ff ff ff ff").unwrap();
    for size in 0..bytes.len() {
        assert!(decode::decode(&bytes[..size], 0, None).is_err());
    }
}

#[test]
#[cfg(target_arch = "x86_64")]
fn half_vector_loads_and_stores_match_native_and_use_one_eight_byte_callback() {
    let p = decode::decode(
        &input::hex(
            "0f 12 03 0f 13 03 0f 16 03 0f 17 03 66 0f 12 03 66 0f 13 03 66 0f 16 03 66 0f 17 03",
        )
        .unwrap(),
        0,
        None,
    )
    .unwrap();
    let mut c = emit::c(&p).unwrap();
    c.push_str(r#"
#include <emmintrin.h>
#include <assert.h>
#include <string.h>
typedef struct { uint64_t words[2]; unsigned loads,stores; } memory;
static uint64_t load(void *ctx,uint64_t a,unsigned n) { memory *mem=ctx;assert(a==128&&n==8);mem->loads++;return mem->words[0]; }
static void store(void *ctx,uint64_t a,unsigned n,uint64_t v) { memory *mem=ctx;assert(a==128&&n==8);mem->stores++;mem->words[0]=v; }
int main(void) {
 uint64_t seed=17;
 for(unsigned trial=0;trial<512;trial++) {
  uint64_t aa[2],bb[2];
  for(unsigned j=0;j<2;j++){seed=seed*UINT64_C(6364136223846793005)+1;aa[j]=seed;seed=seed*UINT64_C(6364136223846793005)+1;bb[j]=seed;}
  if(trial==0){aa[0]=0;aa[1]=UINT64_MAX;bb[0]=UINT64_C(0x123456789abcdef0);bb[1]=UINT64_C(0xfedcba9876543210);}
"#);
    for i in &p.instructions {
        let loading = matches!(i.operands[0], Operand::Vec(_));
        writeln!(c, "{{memory mem={{{{bb[0],bb[1]}},0,0}};uint64_t reference[2]={{bb[0],bb[1]}};__m128i expected;memcpy(&expected,aa,16);rosemary_machine m={{0}};m.x[0]=(rosemary_vec){{aa[0],aa[1]}};m.r[3]=128;m.ctx=&mem;m.load=load;m.store=store;m.rip={};m.cf=m.of=m.zf=m.sf=m.pf=m.df=1;assert(rosemary_run(&m,1)==1);", i.address).unwrap();
        if loading {
            writeln!(
                c,
                "__asm__ volatile(\"{} %1,%0\":\"+x\"(expected):\"m\"(reference[0]):\"memory\");",
                i.op
            )
            .unwrap();
        } else {
            writeln!(
                c,
                "__asm__ volatile(\"{} %1,%0\":\"+m\"(reference[0]):\"x\"(expected):\"memory\");",
                i.op
            )
            .unwrap();
        }
        writeln!(c, "uint64_t words[2];memcpy(words,&expected,16);assert(m.x[0].lo==words[0]&&m.x[0].hi==words[1]);assert(mem.words[0]==reference[0]&&mem.words[1]==reference[1]&&mem.loads=={}&&mem.stores=={});assert(m.r[3]==128&&m.cf&&m.of&&m.zf&&m.sf&&m.pf&&m.df);}}",u8::from(loading),u8::from(!loading)).unwrap();
    }
    c.push_str("}return 0;}\n");
    let dir = std::env::temp_dir().join(format!("rosemary-vector-halves-{}", std::process::id()));
    fs::create_dir_all(&dir).unwrap();
    fs::write(dir.join("native.c"), c).unwrap();
    let output = Command::new("cc")
        .args(["-std=c11", "-O2", "-msse2", "-Wall", "-Wextra", "-Werror"])
        .arg(dir.join("native.c"))
        .arg("-o")
        .arg(dir.join("native"))
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(Command::new(dir.join("native")).status().unwrap().success());
    fs::write(dir.join("native.s"), emit::assembly(&p).unwrap()).unwrap();
    assert!(
        Command::new("cc")
            .arg("-c")
            .arg(dir.join("native.s"))
            .arg("-o")
            .arg(dir.join("native.o"))
            .status()
            .unwrap()
            .success()
    );
    fs::remove_dir_all(dir).unwrap();
}
