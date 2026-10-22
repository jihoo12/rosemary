use rosemary::{
    dataflow::{self, Flags, State},
    decode, emit, input,
    ir::{Operand, Program},
};

#[test]
fn packed_lane_operations_decode_registers_memory_and_preserve_scalar_state() {
    for (opcode, op) in [
        ("da", "pminub"),
        ("de", "pmaxub"),
        ("ea", "pminsw"),
        ("ee", "pmaxsw"),
        ("dc", "paddusb"),
        ("dd", "paddusw"),
        ("d8", "psubusb"),
        ("d9", "psubusw"),
        ("ec", "paddsb"),
        ("ed", "paddsw"),
        ("e8", "psubsb"),
        ("e9", "psubsw"),
        ("e0", "pavgb"),
        ("e3", "pavgw"),
    ] {
        for (encoding, operands) in [
            (
                format!("66 0f {opcode} c1"),
                vec![Operand::Vec(0), Operand::Vec(1)],
            ),
            (
                format!("66 45 0f {opcode} f7"),
                vec![Operand::Vec(14), Operand::Vec(15)],
            ),
        ] {
            let p = decode::decode(&input::hex(&encoding).unwrap(), 0, None).unwrap();
            assert_eq!(p.instructions[0].op, op);
            assert_eq!(p.instructions[0].operands, operands);
            assert_eq!(Program::from_text(&p.to_text().unwrap()).unwrap(), p);
        }
        let p = decode::decode(
            &input::hex(&format!("66 44 0f {opcode} 43 f0")).unwrap(),
            0,
            None,
        )
        .unwrap();
        assert!(
            emit::assembly(&p)
                .unwrap()
                .contains(&format!("{op} xmm8, xmmword ptr [rbx"))
        );
        let before = State {
            registers: [Some(42); 16],
            flags: Flags {
                cf: Some(true),
                zf: Some(false),
                of: Some(true),
                sf: Some(false),
                pf: Some(true),
            },
            direction: Some(true),
        };
        assert_eq!(dataflow::transfer(&p.instructions[0], before), before);
        for encoding in [
            format!("0f {opcode} c1"),
            format!("f3 0f {opcode} c1"),
            format!("f0 66 0f {opcode} c1"),
            format!("66 0f {opcode}"),
        ] {
            assert!(
                decode::decode(&input::hex(&encoding).unwrap(), 0, None).is_err(),
                "{encoding}"
            );
        }
        for operands in [
            "x:128:0 i:64:1",
            "r:64:0 x:128:1",
            "x:128:0 m:64:0:-:1:0",
            "m:128:0:-:1:0 x:128:1",
        ] {
            assert!(
                Program::from_text(&format!("rosemary-ir 1\nentry 0\n0 1 {op} {operands}\n"))
                    .is_err()
            );
        }
    }
    for op in [
        "pminuw", "pmaxsd", "pminsb", "pmaxuq", "paddsd", "psubusq", "pavgd",
    ] {
        assert!(
            Program::from_text(&format!(
                "rosemary-ir 1\nentry 0\n0 1 {op} x:128:0 x:128:1\n"
            ))
            .is_err()
        );
    }
}

#[test]
#[cfg(target_arch = "x86_64")]
fn byte_saturation_and_rounded_average_match_native_for_every_input_pair() {
    use rosemary::ir::Instruction;
    use std::{fmt::Write, fs, process::Command};
    let operations = [
        ("paddusb", "_mm_adds_epu8"),
        ("psubusb", "_mm_subs_epu8"),
        ("paddsb", "_mm_adds_epi8"),
        ("psubsb", "_mm_subs_epi8"),
        ("pavgb", "_mm_avg_epu8"),
    ];
    let p = Program {
        entry: 0,
        instructions: operations
            .iter()
            .enumerate()
            .map(|(n, (op, _))| Instruction {
                address: n as u64,
                len: 1,
                op: (*op).into(),
                operands: vec![Operand::Vec(0), Operand::Vec(1)],
            })
            .collect(),
    };
    let mut c = emit::c(&p).unwrap();
    c.push_str(
        r#"
#include <emmintrin.h>
#include <assert.h>
#include <string.h>
int main(void) {
 for (unsigned a=0;a<256;a++) for (unsigned b=0;b<256;b++) {
  uint64_t left=UINT64_C(0x0101010101010101)*a,right=UINT64_C(0x0101010101010101)*b;
  __m128i first=_mm_set1_epi8((char)a),second=_mm_set1_epi8((char)b);
"#,
    );
    for (n, (_, intrinsic)) in operations.iter().enumerate() {
        writeln!(c,"{{rosemary_machine m={{0}};m.x[0]=(rosemary_vec){{left,left}};m.x[1]=(rosemary_vec){{right,right}};m.rip={n};m.cf=m.zf=m.sf=m.of=m.pf=m.df=1;assert(rosemary_run(&m,1)==1);__m128i expected={intrinsic}(first,second);uint64_t words[2];memcpy(words,&expected,16);assert(m.x[0].lo==words[0]&&m.x[0].hi==words[1]);assert(m.cf&&m.zf&&m.sf&&m.of&&m.pf&&m.df);}} ").unwrap();
    }
    c.push_str("}return 0;}\n");
    let dir =
        std::env::temp_dir().join(format!("rosemary-saturation-native-{}", std::process::id()));
    fs::create_dir_all(&dir).unwrap();
    fs::write(dir.join("test.c"), c).unwrap();
    let output = Command::new("cc")
        .args(["-std=c11", "-O2", "-msse2", "-Wall", "-Wextra", "-Werror"])
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
