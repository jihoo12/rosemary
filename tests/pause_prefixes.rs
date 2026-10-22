use rosemary::{
    dataflow::{self, Flags, State},
    decode, emit,
    ir::{Operand, Program},
};
#[cfg(target_arch = "x86_64")]
use std::{fmt::Write, fs, process::Command};

#[test]
fn pause_ignores_rex_bits_while_plain_90_retains_extended_exchange() {
    let state = State {
        registers: std::array::from_fn(|n| Some(0x1234567800000000 + n as u64)),
        flags: Flags {
            zf: Some(true),
            sf: Some(false),
            cf: Some(true),
            of: Some(false),
            pf: Some(true),
        },
        direction: Some(true),
    };
    for rex in 0x40..=0x4f {
        let p = decode::decode(&[0xf3, rex, 0x90], 0x1000, None).unwrap();
        let i = &p.instructions[0];
        assert_eq!(i.op, "pause", "REX {rex:02x}");
        assert!(i.operands.is_empty());
        assert_eq!(i.len, 3);
        assert_eq!(Program::from_text(&p.to_text().unwrap()).unwrap(), p);
        assert_eq!(dataflow::transfer(i, state), state);
        let plain = decode::decode(&[rex, 0x90], 0, None).unwrap();
        if rex & 1 == 0 {
            assert_eq!(plain.instructions[0].op, "nop");
        } else {
            let width = if rex & 8 != 0 { 64 } else { 32 };
            assert_eq!(plain.instructions[0].op, "xchg");
            assert_eq!(
                plain.instructions[0].operands,
                [Operand::Reg(0, width), Operand::Reg(8, width)]
            );
        }
    }
    for bytes in [
        &[0xf3, 0x90][..],
        &[0x64, 0xf3, 0x41, 0x90],
        &[0xf3, 0x65, 0x49, 0x90],
    ] {
        let p = decode::decode(bytes, 0, None).unwrap();
        assert_eq!(p.instructions[0].op, "pause");
        assert_eq!(p.instructions[0].len as usize, bytes.len());
    }
}

#[test]
#[cfg(target_arch = "x86_64")]
fn rex_pause_preserves_c_and_native_registers_and_all_modeled_flags() {
    let bytes: Vec<_> = (0x40..=0x4f).flat_map(|rex| [0xf3, rex, 0x90]).collect();
    let p = decode::decode(&bytes, 0x1000, None).unwrap();
    let mut c = emit::c(&p).unwrap();
    c.push_str(r#"
#include <assert.h>
#include <string.h>
int main(void){for(unsigned trial=0;trial<64;trial++){
 uint64_t accumulator=UINT64_C(0xaaaaaaaa12345678)+trial,other=UINT64_C(0xbbbbbbbb87654321)-trial;
 uint64_t desired=2|(trial&1)|((trial&2)<<1)|((trial&4)<<4)|((trial&8)<<4)|((trial&16)<<6)|((trial&32)<<6);
"#);
    for (i, rex) in p.instructions.iter().zip(0x40..=0x4f) {
        writeln!(c,"{{rosemary_machine m={{0}};for(unsigned j=0;j<16;j++){{m.r[j]=UINT64_C(0x1234567800000000)+j;m.x[j]=(rosemary_vec){{j,~(uint64_t)j}};}}m.r[0]=accumulator;m.r[8]=other;m.fs_base=42;m.gs_base=57;m.rip={};m.cf=desired&1;m.pf=(desired>>2)&1;m.zf=(desired>>6)&1;m.sf=(desired>>7)&1;m.df=(desired>>10)&1;m.of=(desired>>11)&1;rosemary_machine expected=m;assert(rosemary_run(&m,0)==1&&memcmp(&m,&expected,sizeof(m))==0);expected.rip={};assert(rosemary_run(&m,1)==1&&memcmp(&m,&expected,sizeof(m))==0);uint64_t native=accumulator,flags;register uint64_t native_r8 __asm__(\"r8\")=other;__asm__ volatile(\"pushq %3;popfq;.byte 0xf3,0x{rex:x},0x90;pushfq;popq %2;cld\":\"+a\"(native),\"+r\"(native_r8),\"=r\"(flags):\"r\"(desired):\"cc\",\"memory\");assert(native==accumulator&&native_r8==other&&((flags^desired)&UINT64_C(0xcd5))==0);}}",i.address,i.address+u64::from(i.len)).unwrap();
    }
    c.push_str("}return 0;}\n");
    let dir = std::env::temp_dir().join(format!("rosemary-pause-prefixes-{}", std::process::id()));
    fs::create_dir_all(&dir).unwrap();
    fs::write(dir.join("test.c"), c).unwrap();
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
    fs::write(dir.join("test.s"), emit::assembly(&p).unwrap()).unwrap();
    assert!(
        Command::new("cc")
            .arg("-c")
            .arg(dir.join("test.s"))
            .arg("-o")
            .arg(dir.join("test.o"))
            .status()
            .unwrap()
            .success()
    );
    fs::remove_dir_all(dir).unwrap();
}
