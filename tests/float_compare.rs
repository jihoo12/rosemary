use rosemary::{
    dataflow::{self, Flags, State},
    decode, emit,
    ir::{Operand, Program, Segment},
};
#[cfg(all(target_arch = "x86_64", target_os = "linux"))]
use std::{fmt::Write, fs, process::Command};

const OPS: [(&str, u8, u8, u8); 4] = [
    ("ucomiss", 0, 0x2e, 32),
    ("comiss", 0, 0x2f, 32),
    ("ucomisd", 0x66, 0x2e, 64),
    ("comisd", 0x66, 0x2f, 64),
];

#[test]
fn scalar_compare_encodings_have_exact_widths_and_conservative_flag_facts() {
    let before = State {
        registers: [Some(42); 16],
        flags: Flags {
            zf: Some(true),
            sf: Some(true),
            cf: Some(true),
            of: Some(true),
            pf: Some(true),
        },
        direction: Some(true),
    };
    for (op, prefix, opcode, width) in OPS {
        for dst in 0..16 {
            for src in 0..16 {
                for w in [0, 8] {
                    let mut bytes = Vec::new();
                    if prefix != 0 {
                        bytes.push(prefix);
                    }
                    bytes.extend([
                        0x40 | w | ((dst >> 3) << 2) | (src >> 3),
                        0x0f,
                        opcode,
                        0xc0 | ((dst & 7) << 3) | (src & 7),
                    ]);
                    let p = decode::decode(&bytes, 0x1000, None).unwrap();
                    let i = &p.instructions[0];
                    assert_eq!(i.op, op);
                    assert_eq!(i.operands, [Operand::Vec(dst), Operand::Vec(src)]);
                    assert_eq!(i.len as usize, bytes.len());
                    assert_eq!(Program::from_text(&p.to_text().unwrap()).unwrap(), p);
                    let after = dataflow::transfer(i, before);
                    assert_eq!(after.registers, before.registers);
                    assert_eq!(after.direction, before.direction);
                    assert_eq!(
                        after.flags,
                        Flags {
                            sf: Some(false),
                            of: Some(false),
                            ..Flags::default()
                        }
                    );
                }
            }
        }
        let mut bytes = vec![0x64];
        if prefix != 0 {
            bytes.push(prefix);
        }
        bytes.extend([0x47, 0x0f, opcode, 0x44, 0x6c, 0xf0]);
        let p = decode::decode(&bytes, 0x1000, None).unwrap();
        assert_eq!(
            p.instructions[0].operands,
            [
                Operand::Vec(8),
                Operand::Mem {
                    segment: Some(Segment::Fs),
                    width,
                    base: Some(12),
                    index: Some(13),
                    scale: 2,
                    disp: (-16i64) as u64
                }
            ]
        );
        assert!(emit::assembly(&p).unwrap().contains(if width == 32 {
            "dword ptr fs:"
        } else {
            "qword ptr fs:"
        }));
        let mut bytes = Vec::new();
        if prefix != 0 {
            bytes.push(prefix);
        }
        bytes.extend([0x44, 0x0f, opcode, 0x05, 0xf0, 0xff, 0xff, 0xff]);
        let p = decode::decode(&bytes, 0x1000, None).unwrap();
        assert!(
            matches!(p.instructions[0].operands[1],Operand::Mem {width:w,base:None,index:None,disp,..} if w==width&&disp==0x1000+bytes.len() as u64-16)
        );
        for size in 0..bytes.len() {
            assert!(decode::decode(&bytes[..size], 0, None).is_err());
        }
        for args in [
            format!("x:128:0 m:{}:0:-:1:0", if width == 32 { 64 } else { 32 }),
            "m:64:0:-:1:0 x:128:1".into(),
            "r:64:0 x:128:1".into(),
            "x:128:0 i:64:0".into(),
            "x:128:0".into(),
        ] {
            assert!(
                Program::from_text(&format!("rosemary-ir 1\nentry 0\n0 1 {op} {args}\n")).is_err()
            );
        }
    }
    for bytes in [
        vec![0xf2, 0x0f, 0x2e, 0xc1],
        vec![0xf3, 0x0f, 0x2f, 0xc1],
        vec![0xf0, 0x0f, 0x2e, 0x00],
    ] {
        assert!(decode::decode(&bytes, 0, None).is_err());
    }
    let p=Program::from_text("rosemary-ir 1\nentry 0\n0 1 xor r:32:0 r:32:0\n1 1 ucomisd x:128:0 x:128:1\n2 1 jo i:64:4\n3 1 jb i:64:4\n4 1 ret\n").unwrap();
    let a = dataflow::analyze(&p).unwrap();
    assert_eq!(a.branch(2), Some(false));
    assert_eq!(a.branch(3), None);
}

#[test]
#[cfg(all(target_arch = "x86_64", target_os = "linux"))]
fn scalar_comparisons_match_native_mxcsr_flags_memory_and_unmasked_traps() {
    let mut bytes = Vec::new();
    for (_, prefix, opcode, _) in OPS {
        for suffix in [vec![0xc1], vec![0xc0], vec![0x03]] {
            if prefix != 0 {
                bytes.push(prefix);
            }
            bytes.extend([0x0f, opcode]);
            bytes.extend(suffix);
        }
        if prefix != 0 {
            bytes.push(prefix);
        }
        bytes.extend([0x45, 0x0f, opcode, 0xc1]);
        bytes.push(0x64);
        if prefix != 0 {
            bytes.push(prefix);
        }
        bytes.extend([0x47, 0x0f, opcode, 0x44, 0x6c, 0xf0]);
    }
    let p = decode::decode(&bytes, 0x1000, None).unwrap();
    let mut c = emit::c(&p).unwrap();
    c.push_str(NATIVE_HELPERS);
    c.push_str(r#"
int main(void){struct sigaction action={0};action.sa_sigaction=trap_handler;action.sa_flags=SA_SIGINFO;sigemptyset(&action.sa_mask);assert(sigaction(SIGFPE,&action,0)==0);
 unsigned saved=_mm_getcsr();_Alignas(16) unsigned char fx[512];__asm__ volatile("fxsave %0":"=m"(fx));uint32_t supported;memcpy(&supported,fx+28,4);if(!supported)supported=0xffbf;
 const uint64_t singles[]={0,0x80000000,1,0x80000001,0x007fffff,0x807fffff,0x00800000,0x80800000,0x3f800000,0xbf800000,0x7f7fffff,0xff7fffff,0x7f800000,0xff800000,0x7fc00000,0xffc00042,0x7f800001,0xff800001};
 const uint64_t doubles[]={0,UINT64_C(0x8000000000000000),1,UINT64_C(0x8000000000000001),UINT64_C(0x000fffffffffffff),UINT64_C(0x800fffffffffffff),UINT64_C(0x0010000000000000),UINT64_C(0x8010000000000000),UINT64_C(0x3ff0000000000000),UINT64_C(0xbff0000000000000),UINT64_C(0x7fefffffffffffff),UINT64_C(0xffefffffffffffff),UINT64_C(0x7ff0000000000000),UINT64_C(0xfff0000000000000),UINT64_C(0x7ff8000000000000),UINT64_C(0xfff8000000000042),UINT64_C(0x7ff0000000000001),UINT64_C(0xfff0000000000001)};
 uint64_t seed=73;
 for(unsigned environment=0;environment<64;environment++){
  unsigned csr=(0x1f80&~((environment&1?0x80:0)|(environment&2?0x100:0)))|(environment&4?64:0)|(environment&8?0x8000:0)|((environment>>4)<<13);
  if((csr&64)&&!(supported&64))continue;
  for(unsigned trial=0;trial<18*18+256;trial++){
   uint64_t left32,right32,left64,right64;
   if(trial<18*18){left32=singles[trial/18];right32=singles[trial%18];left64=doubles[trial/18];right64=doubles[trial%18];}
   else{seed=seed*UINT64_C(6364136223846793005)+1;left64=seed;left32=seed;seed=seed*UINT64_C(6364136223846793005)+1;right64=seed;right32=seed;}
   unsigned initial_csr=csr|(trial&1?63:0);uint64_t initial_flags=2|((uint64_t)(trial&31)<<0);
   initial_flags=2|(trial&1)|((trial&2)<<1)|((trial&4)<<4)|((trial&8)<<4)|((trial&16)<<7);
"#);
    for i in &p.instructions {
        let width = if i.op.ends_with("ss") { 32 } else { 64 };
        let native_op = OPS.iter().position(|(op, ..)| *op == i.op).unwrap();
        let Operand::Vec(dst) = i.operands[0] else {
            unreachable!()
        };
        let memory = matches!(i.operands[1], Operand::Mem { .. });
        let alias = i.operands[0] == i.operands[1];
        writeln!(c,"{{uint64_t a=left{width},b={};memory mem={{b,{},0}};rosemary_machine m={{0}};for(unsigned j=0;j<16;j++){{m.r[j]=j*17;m.x[j]=(rosemary_vec){{UINT64_C(0x0123456789abcdef),UINT64_C(0xfedcba9876543210)}};}}m.r[3]=128;m.r[12]=80;m.r[13]=16;m.fs_base=32;m.rip={};m.ctx=&mem;m.load=load;m.mxcsr=initial_csr;m.x[{dst}].lo=a;",if alias{format!("left{width}")}else{format!("right{width}")},width/8,i.address).unwrap();
        if let Operand::Vec(src) = i.operands[1]
            && !alias
        {
            writeln!(c, "m.x[{src}].lo=b;").unwrap();
        }
        c.push_str("m.cf=initial_flags&1;m.pf=(initial_flags>>2)&1;m.zf=(initial_flags>>6)&1;m.sf=(initial_flags>>7)&1;m.of=(initial_flags>>11)&1;m.df=trial&1;rosemary_machine before=m;assert(rosemary_run(&m,0)==1&&memcmp(&m,&before,sizeof(m))==0&&mem.loads==0);int status=rosemary_run(&m,1);");
        writeln!(c,"native_result expected=compare_native({native_op},{},a,b,initial_csr,initial_flags);assert(status==(expected.trapped?2:1));assert(m.mxcsr==expected.csr&&m.cf==(expected.flags&1)&&m.pf==((expected.flags>>2)&1)&&m.zf==((expected.flags>>6)&1)&&m.sf==((expected.flags>>7)&1)&&m.of==((expected.flags>>11)&1));assert(mem.loads=={}&&m.rip==(expected.trapped?{}:{}));assert(memcmp(m.x,before.x,sizeof(m.x))==0&&memcmp(m.r,before.r,sizeof(m.r))==0&&m.df==before.df&&m.fs_base==32);if(expected.trapped){{m.mxcsr|=0x180;mem.loads=0;assert(rosemary_run(&m,1)==1&&m.rip=={});}}}}",u8::from(memory),u8::from(memory),i.address,i.address+u64::from(i.len),i.address+u64::from(i.len)).unwrap();
    }
    c.push_str("}}_mm_setcsr(saved);return 0;}\n");
    let dir = std::env::temp_dir().join(format!("rosemary-float-compare-{}", std::process::id()));
    fs::create_dir_all(&dir).unwrap();
    fs::write(dir.join("test.c"), c).unwrap();
    let output = Command::new("cc")
        .args([
            "-D_GNU_SOURCE",
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

#[cfg(all(target_arch = "x86_64", target_os = "linux"))]
const NATIVE_HELPERS: &str = r#"
#include <assert.h>
#include <string.h>
#include <signal.h>
#include <setjmp.h>
#include <ucontext.h>
#include <emmintrin.h>
typedef struct{uint64_t bits;unsigned width,loads;} memory;
static uint64_t load(void *ctx,uint64_t address,unsigned width){memory *mem=ctx;assert(address==128&&width==mem->width&&mem->loads++==0);return width==4?(uint32_t)mem->bits:mem->bits;}
typedef struct{uint64_t flags;unsigned csr,trapped;} native_result;
static sigjmp_buf recovery;
static volatile uint64_t fault_flags;
static volatile unsigned fault_csr;
static void trap_handler(int signal,siginfo_t *info,void *context){(void)signal;(void)info;ucontext_t *u=context;fault_flags=(uint64_t)u->uc_mcontext.gregs[REG_EFL];fault_csr=u->uc_mcontext.fpregs->mxcsr;siglongjmp(recovery,1);}
#define NATIVE(OP,SOURCE) __asm__ volatile("ldmxcsr %[csr];pushq %[initial];popfq;" OP " " SOURCE ",%[a];pushfq;popq %[flags];stmxcsr %[after];ldmxcsr %[saved]" :[flags]"=r"(flags),[after]"=m"(after):[a]"x"(va),[b]"x"(vb),[memory]"m"(bb),[csr]"m"(csr),[saved]"m"(saved),[initial]"r"(initial):"cc","memory")
static native_result compare_native(unsigned op,unsigned memory,uint64_t a,uint64_t b,unsigned csr,uint64_t initial){
 unsigned saved=_mm_getcsr();uint64_t aa[2]={a,0},bb[2]={b,0};__m128i va,vb;memcpy(&va,aa,16);memcpy(&vb,bb,16);
 if(sigsetjmp(recovery,1)){_mm_setcsr(saved);return(native_result){fault_flags,fault_csr,1};}
 uint64_t flags;unsigned after;
 switch(op){
 case 0:if(memory){NATIVE("ucomiss","%[memory]");}else{NATIVE("ucomiss","%[b]");}break;
 case 1:if(memory){NATIVE("comiss","%[memory]");}else{NATIVE("comiss","%[b]");}break;
 case 2:if(memory){NATIVE("ucomisd","%[memory]");}else{NATIVE("ucomisd","%[b]");}break;
 default:if(memory){NATIVE("comisd","%[memory]");}else{NATIVE("comisd","%[b]");}break;
 }
 return(native_result){flags,after,0};
}
"#;
