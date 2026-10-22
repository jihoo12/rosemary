use rosemary::{
    dataflow::{self, Flags, State},
    decode, emit,
    ir::{Operand, Program, Segment},
};
#[cfg(target_arch = "x86_64")]
use std::{fmt::Write, fs, process::Command};

const OPS: [(&str, u8); 3] = [("packsswb", 0x63), ("packssdw", 0x6b), ("packuswb", 0x67)];

#[test]
fn packing_registers_memory_and_prefixes_preserve_metadata() {
    let state = State {
        registers: std::array::from_fn(|r| Some(r as u64 * 17)),
        flags: Flags {
            zf: Some(true),
            sf: Some(false),
            cf: Some(true),
            of: Some(false),
            pf: Some(true),
        },
        direction: Some(true),
    };
    for (op, opcode) in OPS {
        for dst in 0..16 {
            for src in 0..16 {
                for w in [0, 8] {
                    let rex = 0x40 | w | ((dst >> 3) << 2) | (src >> 3);
                    let p = decode::decode(
                        &[0x66, rex, 0x0f, opcode, 0xc0 | ((dst & 7) << 3) | (src & 7)],
                        0x1000,
                        None,
                    )
                    .unwrap();
                    let i = &p.instructions[0];
                    assert_eq!(i.op, op);
                    assert_eq!(i.operands, [Operand::Vec(dst), Operand::Vec(src)]);
                    assert_eq!(i.len, 5);
                    assert_eq!(Program::from_text(&p.to_text().unwrap()).unwrap(), p);
                    assert_eq!(dataflow::transfer(i, state), state);
                }
            }
        }
        let p = decode::decode(
            &[0x64, 0x66, 0x47, 0x0f, opcode, 0x44, 0x6c, 0xf0],
            0x1000,
            None,
        )
        .unwrap();
        assert_eq!(
            p.instructions[0].operands,
            [
                Operand::Vec(8),
                Operand::Mem {
                    segment: Some(Segment::Fs),
                    width: 128,
                    base: Some(12),
                    index: Some(13),
                    scale: 2,
                    disp: (-16i64) as u64
                }
            ]
        );
        assert!(emit::assembly(&p).unwrap().contains("xmmword ptr fs:"));
        let bytes = [0x66, 0x44, 0x0f, opcode, 0x05, 0xf0, 0xff, 0xff, 0xff];
        let p = decode::decode(&bytes, 0x1000, None).unwrap();
        assert_eq!(
            p.instructions[0].operands,
            [
                Operand::Vec(8),
                Operand::Mem {
                    segment: None,
                    width: 128,
                    base: None,
                    index: None,
                    scale: 1,
                    disp: 0xff9
                }
            ]
        );
        for size in 0..bytes.len() {
            assert!(decode::decode(&bytes[..size], 0, None).is_err());
        }
        for prefix in [vec![], vec![0xf2], vec![0xf3], vec![0xf0, 0x66]] {
            let mut bytes = prefix;
            bytes.extend([0x0f, opcode, 0xc1]);
            assert!(decode::decode(&bytes, 0, None).is_err());
        }
        for operands in [
            "m:128:0:-:1:0 x:128:1",
            "x:128:0 m:64:0:-:1:0",
            "x:128:0 r:64:0",
            "x:128:0 i:8:1",
            "x:128:0",
            "x:128:0 x:128:1 i:8:0",
        ] {
            assert!(
                Program::from_text(&format!("rosemary-ir 1\nentry 0\n0 1 {op} {operands}\n"))
                    .is_err(),
                "{op} {operands}"
            );
        }
    }
}

#[test]
#[cfg(target_arch = "x86_64")]
fn saturated_packing_matches_native_for_all_word_inputs_and_dword_boundaries() {
    let mut bytes = Vec::new();
    for (_, opcode) in OPS {
        bytes.extend([0x66, 0x0f, opcode, 0xc1]); // xmm0,xmm1
        bytes.extend([0x66, 0x0f, opcode, 0xc0]); // xmm0,xmm0
        bytes.extend([0x66, 0x45, 0x0f, opcode, 0xc1]); // xmm8,xmm9
        bytes.extend([0x66, 0x0f, opcode, 0x03]); // xmm0,[rbx]
        bytes.extend([0x64, 0x66, 0x47, 0x0f, opcode, 0x44, 0x6c, 0xf0]); // xmm8,fs:[r12+r13*2-16]
    }
    let p = decode::decode(&bytes, 0x1000, None).unwrap();
    let mut c = emit::c(&p).unwrap();
    c.push_str(r#"
#include <emmintrin.h>
#include <assert.h>
#include <string.h>
typedef struct {uint64_t words[2];unsigned loads;} memory;
static uint64_t load(void *ctx,uint64_t address,unsigned width){memory *mem=ctx;assert(width==8&&mem->loads<2&&address==128+8*mem->loads);return mem->words[mem->loads++];}
int main(void){
 const uint32_t edges[]={0,1,126,127,128,129,254,255,256,257,32766,32767,32768,32769,0xffff7fff,0xffff8000,0xffff8001,0xffffff7f,0xffffff80,0xffffff81,0x7fffffff,0x80000000,0xffffffff};
 const unsigned edge_count=sizeof(edges)/sizeof(edges[0]);
 for(unsigned trial=0;trial<65536+edge_count;trial++){
  uint64_t aa[2]={0},bb[2]={0};
  if(trial<edge_count){for(unsigned j=0;j<4;j++){aa[j/2]|=(uint64_t)edges[(trial+j)%edge_count]<<(32*(j%2));bb[j/2]|=(uint64_t)edges[(trial+7+j)%edge_count]<<(32*(j%2));}}
  else{for(unsigned j=0;j<8;j++){aa[j/4]|=(uint64_t)((trial-edge_count+j*8191)&65535)<<(16*(j%4));bb[j/4]|=(uint64_t)((65535-(trial-edge_count)+j*4093)&65535)<<(16*(j%4));}}
"#);
    for i in &p.instructions {
        let Operand::Vec(dst) = i.operands[0] else {
            unreachable!()
        };
        let alias = i.operands[0] == i.operands[1];
        let memory = matches!(i.operands[1], Operand::Mem { .. });
        writeln!(c,"{{memory mem={{{{bb[0],bb[1]}},0}};rosemary_machine m={{0}};for(unsigned j=0;j<16;j++){{m.r[j]=j*17;m.x[j]=(rosemary_vec){{aa[0],aa[1]}};}}m.r[3]=128;m.r[12]=80;m.r[13]=16;m.fs_base=32;m.ctx=&mem;m.load=load;m.rip={};",i.address).unwrap();
        if let Operand::Vec(src) = i.operands[1] {
            writeln!(c, "m.x[{src}]=(rosemary_vec){{bb[0],bb[1]}};").unwrap();
        }
        c.push_str("rosemary_vec before[16];uint64_t registers[16];memcpy(before,m.x,sizeof(before));memcpy(registers,m.r,sizeof(registers));unsigned flag=trial&1;m.cf=m.pf=m.zf=m.sf=m.of=m.df=flag;__m128i expected;memcpy(&expected,aa,16);assert(rosemary_run(&m,0)==1&&mem.loads==0);assert(rosemary_run(&m,1)==1);");
        if alias {
            c.push_str("memcpy(&expected,bb,16);");
            writeln!(c, "__asm__ volatile(\"{} %0,%0\":\"+x\"(expected));", i.op).unwrap();
        } else if memory {
            writeln!(
                c,
                "__asm__ volatile(\"{} %1,%0\":\"+x\"(expected):\"m\"(bb):\"memory\");",
                i.op
            )
            .unwrap();
        } else {
            c.push_str("__m128i source;memcpy(&source,bb,16);");
            writeln!(
                c,
                "__asm__ volatile(\"{} %1,%0\":\"+x\"(expected):\"x\"(source));",
                i.op
            )
            .unwrap();
        }
        writeln!(c,"uint64_t words[2];memcpy(words,&expected,16);assert(m.x[{dst}].lo==words[0]&&m.x[{dst}].hi==words[1]);for(unsigned j=0;j<16;j++)if(j!={dst})assert(memcmp(&m.x[j],&before[j],sizeof(before[j]))==0);assert(mem.loads=={}&&mem.words[0]==bb[0]&&mem.words[1]==bb[1]&&memcmp(m.r,registers,sizeof(registers))==0);assert(m.cf==flag&&m.pf==flag&&m.zf==flag&&m.sf==flag&&m.of==flag&&m.df==flag&&m.rip=={});}}",if memory {2} else {0},i.address+u64::from(i.len)).unwrap();
    }
    c.push_str("}return 0;}\n");
    let dir = std::env::temp_dir().join(format!("rosemary-vector-packing-{}", std::process::id()));
    fs::create_dir_all(&dir).unwrap();
    fs::write(dir.join("test.c"), c).unwrap();
    let output = Command::new("cc")
        .args([
            "-std=c11",
            "-O2",
            "-msse2",
            "-fsanitize=undefined",
            "-fno-sanitize-recover=all",
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
