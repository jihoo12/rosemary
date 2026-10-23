use rosemary::{
    dataflow::{self, State},
    decode,
    dynamic::{self, Machine, Options, Stop},
    emit,
    ir::{Operand, Program, Segment},
};
#[cfg(target_arch = "x86_64")]
use std::{fmt::Write, fs, process::Command};

const OPS: [(&str, &[u8]); 17] = [
    ("pmaddwd", &[0x0f, 0xf5]),
    ("psadbw", &[0x0f, 0xf6]),
    ("pshufb", &[0x0f, 0x38, 0x00]),
    ("pmaddubsw", &[0x0f, 0x38, 0x04]),
    ("pabsb", &[0x0f, 0x38, 0x1c]),
    ("pabsw", &[0x0f, 0x38, 0x1d]),
    ("pabsd", &[0x0f, 0x38, 0x1e]),
    ("psignb", &[0x0f, 0x38, 0x08]),
    ("psignw", &[0x0f, 0x38, 0x09]),
    ("psignd", &[0x0f, 0x38, 0x0a]),
    ("phaddw", &[0x0f, 0x38, 0x01]),
    ("phaddd", &[0x0f, 0x38, 0x02]),
    ("phaddsw", &[0x0f, 0x38, 0x03]),
    ("phsubw", &[0x0f, 0x38, 0x05]),
    ("phsubd", &[0x0f, 0x38, 0x06]),
    ("phsubsw", &[0x0f, 0x38, 0x07]),
    ("pmulhrsw", &[0x0f, 0x38, 0x0b]),
];

#[test]
fn byte_vector_encodings_preserve_extensions_memory_and_ir_validation() {
    let state = State {
        registers: [Some(17); 16],
        ..State::default()
    };
    for (op, opcode) in OPS {
        for dst in 0..16 {
            for src in 0..16 {
                for wide in [0, 8] {
                    let mut bytes = vec![0x66, 0x40 | wide | ((dst >> 3) << 2) | (src >> 3)];
                    bytes.extend(opcode);
                    bytes.push(0xc0 | ((dst & 7) << 3) | (src & 7));
                    let p = decode::decode(&bytes, 0x1000, None).unwrap();
                    let i = &p.instructions[0];
                    assert_eq!(i.op, op);
                    assert_eq!(i.operands, [Operand::Vec(dst), Operand::Vec(src)]);
                    assert_eq!(i.len as usize, bytes.len());
                    assert_eq!(dataflow::transfer(i, state), state);
                    assert_eq!(Program::from_text(&p.to_text().unwrap()).unwrap(), p);
                }
            }
        }
        let mut bytes = vec![0x64, 0x66, 0x47];
        bytes.extend(opcode);
        bytes.extend([0x44, 0x6c, 0xf0]);
        let p = decode::decode(&bytes, 0x1000, None).unwrap();
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
        let mut bytes = vec![0x66, 0x44];
        bytes.extend(opcode);
        bytes.extend([0x05, 0xf0, 0xff, 0xff, 0xff]);
        let p = decode::decode(&bytes, 0x1000, None).unwrap();
        let Operand::Mem { disp, base, .. } = p.instructions[0].operands[1] else {
            panic!()
        };
        assert_eq!(base, None);
        assert_eq!(disp, 0x1000 + bytes.len() as u64 - 16);
        for size in 0..bytes.len() {
            assert!(decode::decode(&bytes[..size], 0, None).is_err());
        }
        for prefix in [
            vec![],
            vec![0xf2],
            vec![0xf3],
            vec![0xf0, 0x66],
            vec![0xf3, 0x66],
        ] {
            let mut bytes = prefix;
            bytes.extend(opcode);
            bytes.push(0xc1);
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
                    .is_err()
            );
        }
    }
}

#[test]
fn movbe_widths_flags_extensions_and_concrete_memory_are_preserved() {
    for (prefix, w) in [
        (vec![0x66], 16),
        (vec![], 32),
        (vec![0x48], 64),
        (vec![0x66, 0x48], 64),
    ] {
        for opcode in [0xf0, 0xf1] {
            let mut bytes = prefix.clone();
            bytes.extend([0x0f, 0x38, opcode, 0x03]); // rax,[rbx] or [rbx],rax
            let p = decode::decode(&bytes, 0x1000, None).unwrap();
            let i = &p.instructions[0];
            assert_eq!(i.op, "movbe");
            assert_eq!(i.operands[0].width(), w);
            assert_eq!(Program::from_text(&p.to_text().unwrap()).unwrap(), p);
            let mut m = Machine::with_stack(p.entry);
            m.state.registers[0] = 0x123456789abcdef0;
            m.state.registers[3] = 0x2000;
            m.state.flags.cf = true;
            m.state.flags.df = true;
            let flags = m.state.flags;
            let original = 0x1122334455667788u64;
            m.memory
                .map(0x2000, original.to_le_bytes().to_vec(), true)
                .unwrap();
            let r = dynamic::execute(
                &p,
                &mut m,
                Options {
                    max_steps: 1,
                    trace_limit: 1,
                },
            )
            .unwrap();
            assert_eq!(r.stop, Stop::LeftCode);
            assert_eq!(m.state.flags, flags);
            let mask = u64::MAX >> (64 - w);
            if opcode == 0xf0 {
                let expected = ((original & mask).swap_bytes() >> (64 - w))
                    | if w == 16 { 0x123456789abc0000 } else { 0 };
                assert_eq!(m.state.registers[0], expected);
                assert_eq!(r.memory_reads, 1);
                assert_eq!(r.memory_writes, 0);
            } else {
                assert_eq!(
                    m.memory.load(0x2000, w / 8).unwrap(),
                    (0x123456789abcdef0u64 & mask).swap_bytes() >> (64 - w)
                );
                assert_eq!(r.memory_writes, 1);
                assert_eq!(r.memory_reads, 0);
            }
            let state = State {
                registers: [Some(0x1234); 16],
                ..State::default()
            };
            let after = dataflow::transfer(i, state);
            assert_eq!(after.flags, state.flags);
            assert_eq!(after.direction, state.direction);
            assert_eq!(
                after.registers[0],
                if opcode == 0xf0 {
                    None
                } else {
                    state.registers[0]
                }
            );
        }
    }
    let p = decode::decode(
        &[0x65, 0x66, 0x47, 0x0f, 0x38, 0xf1, 0x44, 0x6c, 0xf0],
        0x1000,
        None,
    )
    .unwrap();
    assert_eq!(p.instructions[0].operands[1], Operand::Reg(8, 16));
    assert!(emit::assembly(&p).unwrap().contains("word ptr gs:"));
    for bytes in [
        vec![0x0f, 0x38, 0xf0, 0xc1],
        vec![0x0f, 0x38, 0xf1, 0xc1],
        vec![0xf0, 0x0f, 0x38, 0xf0, 0x03],
        vec![0xf2, 0x0f, 0x38, 0xf0, 0x03],
        vec![0xf3, 0x0f, 0x38, 0xf1, 0x03],
        vec![0x66, 0x0f, 0x38, 0xff, 0xc0],
    ] {
        assert!(decode::decode(&bytes, 0, None).is_err());
    }
    for operands in [
        "r:64:0 r:64:1",
        "m:64:0:-:1:0 m:64:1:-:1:0",
        "r:8:0 m:8:0:-:1:0",
        "r:32:0 m:16:0:-:1:0",
        "r:64:0 i:64:1",
    ] {
        assert!(
            Program::from_text(&format!("rosemary-ir 1\nentry 0\n0 1 movbe {operands}\n")).is_err()
        );
    }
}

#[test]
#[cfg(target_arch = "x86_64")]
fn vector_byte_operations_match_native() {
    if !std::arch::is_x86_feature_detected!("ssse3") {
        return;
    }
    let mut bytes = Vec::new();
    for (_, opcode) in OPS {
        for (prefix, suffix) in [
            (&[0x66][..], &[0xc1][..]),
            (&[0x66][..], &[0xc0][..]),
            (&[0x66, 0x45][..], &[0xc1][..]),
            (&[0x66][..], &[0x03][..]),
            (&[0x64, 0x66, 0x47][..], &[0x44, 0x6c, 0xf0][..]),
        ] {
            bytes.extend(prefix);
            bytes.extend(opcode);
            bytes.extend(suffix);
        }
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
 const uint16_t boundary[]={0,1,0xffff,0x7fff,0x8000,0x8001,0x00ff,0x0100};
 uint64_t seed=7;
 for(unsigned trial=0;trial<65536;trial++){
  _Alignas(16) uint64_t aa[2]={0},bb[2]={0};
  for(unsigned j=0;j<8;j++){
   unsigned left=(trial+j*8191)&65535,right=boundary[(trial+j)%8];
   if(trial<8)left=right=boundary[trial];
   if(trial>=256){seed=seed*UINT64_C(6364136223846793005)+1;right=(unsigned)(seed>>16)&65535;}
   aa[j/4]|=(uint64_t)left<<(16*(j%4));bb[j/4]|=(uint64_t)right<<(16*(j%4));
  }
  /* Exercise every shuffle mask byte, including bit-7 zeroing and ignored bits. */
  if(trial<256){for(unsigned j=0;j<2;j++)bb[j]=UINT64_C(0x0101010101010101)*trial;}
  if(trial==256){aa[0]=aa[1]=bb[0]=bb[1]=UINT64_C(0x8000800080008000);}
"#);
    for i in &p.instructions {
        let Operand::Vec(dst) = i.operands[0] else {
            unreachable!()
        };
        let alias = i.operands[0] == i.operands[1];
        let memory = matches!(i.operands[1], Operand::Mem { .. });
        writeln!(c, "{{memory mem={{{{bb[0],bb[1]}},0}};rosemary_machine m={{0}};for(unsigned j=0;j<16;j++){{m.r[j]=j*17;m.x[j]=(rosemary_vec){{aa[0],aa[1]}};}}m.r[3]=128;m.r[12]=80;m.r[13]=16;m.fs_base=32;m.ctx=&mem;m.load=load;m.rip={};", i.address).unwrap();
        if let Operand::Vec(src) = i.operands[1] {
            writeln!(c, "m.x[{src}]=(rosemary_vec){{bb[0],bb[1]}};").unwrap();
        }
        c.push_str("rosemary_vec before[16];uint64_t registers[16];memcpy(before,m.x,sizeof(before));memcpy(registers,m.r,sizeof(registers));unsigned flag=trial&1;m.cf=m.pf=m.zf=m.sf=m.of=m.df=flag;__m128i expected;memcpy(&expected,aa,16);assert(rosemary_run(&m,1)==1);");
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
        writeln!(c, "uint64_t words[2];memcpy(words,&expected,16);assert(m.x[{dst}].lo==words[0]&&m.x[{dst}].hi==words[1]);for(unsigned j=0;j<16;j++)if(j!={dst})assert(memcmp(&m.x[j],&before[j],sizeof(before[j]))==0);assert(mem.loads=={}&&mem.words[0]==bb[0]&&mem.words[1]==bb[1]&&memcmp(m.r,registers,sizeof(registers))==0);assert(m.cf==flag&&m.pf==flag&&m.zf==flag&&m.sf==flag&&m.of==flag&&m.df==flag);}}", if memory { 2 } else { 0 }).unwrap();
    }
    c.push_str("}return 0;}\n");
    compile_run("byte-vectors", &p, &c);
}

#[test]
#[cfg(target_arch = "x86_64")]
fn concrete_mixed_byte_products_match_native_for_every_byte_pair() {
    if !std::arch::is_x86_feature_detected!("ssse3") {
        return;
    }
    use std::arch::x86_64::{__m128i, _mm_maddubs_epi16};
    for alias in [false, true] {
        let p = decode::decode(
            &[
                0x66,
                0x45,
                0x0f,
                0x38,
                0x04,
                if alias { 0xc0 } else { 0xc1 },
            ],
            0x1000,
            None,
        )
        .unwrap();
        let mut m = Machine::with_stack(p.entry);
        m.state.xmm.fill(u128::MAX);
        for trial in 0..65536u32 {
            let mut left = [0u8; 16];
            let mut right = [0u8; 16];
            for n in 0..16 {
                left[n] = (trial as u8).wrapping_add((n * 31) as u8);
                right[n] = ((trial >> 8) as u8).wrapping_add((n * 17) as u8);
            }
            // Include uniform extreme products as well as varied lane pairs.
            if trial < 256 {
                left.fill(255);
                right.fill(trial as u8);
            }
            let left = u128::from_le_bytes(left);
            let right = if alias {
                left
            } else {
                u128::from_le_bytes(right)
            };
            // Runtime feature detection above protects this SSSE3 instruction.
            let expected: u128 = unsafe {
                std::mem::transmute(_mm_maddubs_epi16(
                    std::mem::transmute::<u128, __m128i>(left),
                    std::mem::transmute::<u128, __m128i>(right),
                ))
            };
            m.state.rip = p.entry;
            m.state.xmm[8] = left;
            m.state.xmm[9] = right;
            m.state.flags.cf = trial & 1 != 0;
            m.state.flags.of = trial & 2 != 0;
            m.state.flags.df = trial & 4 != 0;
            let before = m.state;
            let r = dynamic::execute(
                &p,
                &mut m,
                Options {
                    max_steps: 1,
                    trace_limit: 0,
                },
            )
            .unwrap();
            assert_eq!(r.stop, Stop::LeftCode);
            let mut after = before;
            after.xmm[8] = expected;
            after.rip += u64::from(p.instructions[0].len);
            assert_eq!(m.state, after, "trial {trial}, alias {alias}");
            assert_eq!((r.memory_reads, r.memory_writes), (0, 0));
        }
    }
}

#[cfg(target_arch = "x86_64")]
fn compile_run(name: &str, p: &Program, c: &str) {
    let dir = std::env::temp_dir().join(format!("rosemary-{name}-{}", std::process::id()));
    fs::create_dir_all(&dir).unwrap();
    fs::write(dir.join("test.c"), c).unwrap();
    fs::write(dir.join("test.s"), emit::assembly(p).unwrap()).unwrap();
    for (input, output, extra) in [
        (
            "test.c",
            "test",
            vec![
                "-std=c11",
                "-O2",
                "-msse2",
                "-fsanitize=undefined",
                "-fno-sanitize-recover=all",
                "-Wall",
                "-Wextra",
                "-Werror",
            ],
        ),
        ("test.s", "test.o", vec!["-c"]),
    ] {
        let result = Command::new("cc")
            .args(extra)
            .arg(dir.join(input))
            .arg("-o")
            .arg(dir.join(output))
            .output()
            .unwrap();
        assert!(
            result.status.success(),
            "{}",
            String::from_utf8_lossy(&result.stderr)
        );
    }
    assert!(Command::new(dir.join("test")).status().unwrap().success());
    fs::remove_dir_all(dir).unwrap();
}

#[test]
#[cfg(target_arch = "x86_64")]
fn movbe_c_matches_native_register_and_memory_forms() {
    if !std::arch::is_x86_feature_detected!("movbe") {
        return;
    }
    let mut bytes = Vec::new();
    for prefix in [&[0x66][..], &[][..], &[0x48][..]] {
        for opcode in [0xf0, 0xf1] {
            bytes.extend(prefix);
            bytes.extend([0x0f, 0x38, opcode, 0x03]);
        }
    }
    let p = decode::decode(&bytes, 0x1000, None).unwrap();
    let mut c = emit::c(&p).unwrap();
    c.push_str(r#"
#include <assert.h>
#include <string.h>
typedef struct {uint64_t value;unsigned width,reads,writes;} memory;
static uint64_t load(void *ctx,uint64_t a,unsigned n){memory *mem=ctx;assert(a==128&&n==mem->width);mem->reads++;return mem->value;}
static void store(void *ctx,uint64_t a,unsigned n,uint64_t v){memory *mem=ctx;assert(a==128&&n==mem->width);mem->writes++;uint64_t mask=rosemary_vmask(n*8);mem->value=(mem->value&~mask)|(v&mask);}
int main(void){uint64_t seed=7;for(unsigned trial=0;trial<4096;trial++){
 seed=seed*UINT64_C(6364136223846793005)+1;uint64_t a=seed;
 seed=seed*UINT64_C(6364136223846793005)+1;uint64_t b=seed;
"#);
    for i in &p.instructions {
        let width = i.operands[0].width();
        let load = matches!(i.operands[0], Operand::Reg(..));
        let suffix = match width {
            16 => "w",
            32 => "l",
            _ => "q",
        };
        writeln!(c, "{{memory mem={{b,{},0,0}};rosemary_machine m={{0}};m.r[0]=a;m.r[3]=128;m.rip={};m.load=load;m.store=store;m.ctx=&mem;m.cf=m.pf=m.zf=m.sf=m.of=m.df=trial&1;assert(rosemary_run(&m,1)==1);uint{width}_t value=(uint{width}_t)a,reference=(uint{width}_t)b;", width/8, i.address).unwrap();
        if load {
            writeln!(c, "__asm__ volatile(\"movbe{suffix} %1,%0\":\"=r\"(value):\"m\"(reference):\"memory\");assert(m.r[0]==((uint64_t)value|{}));assert(mem.value==b&&mem.reads==1&&!mem.writes);", if width==16 { "(a&~UINT64_C(0xffff))" } else { "0" }).unwrap();
        } else {
            writeln!(c, "__asm__ volatile(\"movbe{suffix} %1,%0\":\"=m\"(reference):\"r\"(value):\"memory\");uint64_t mask=rosemary_vmask({width});assert(mem.value==((b&~mask)|(uint64_t)reference));assert(m.r[0]==a&&mem.writes==1&&!mem.reads);").unwrap();
        }
        c.push_str("assert(m.cf==(trial&1)&&m.pf==(trial&1)&&m.zf==(trial&1)&&m.sf==(trial&1)&&m.of==(trial&1)&&m.df==(trial&1));}\n");
    }
    c.push_str("}return 0;}\n");
    compile_run("movbe", &p, &c);
}
