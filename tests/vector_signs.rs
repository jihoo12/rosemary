use rosemary::{
    decode,
    dynamic::{self, Machine, Options, Stop},
};

const ABS: [(&str, u8, u8); 3] = [("pabsb", 0x1c, 8), ("pabsw", 0x1d, 16), ("pabsd", 0x1e, 32)];

fn run(p: &rosemary::ir::Program, m: &mut Machine) -> dynamic::Report {
    dynamic::execute(
        p,
        m,
        Options {
            max_steps: 1,
            trace_limit: 1,
        },
    )
    .unwrap()
}

#[test]
fn absolute_minimums_are_unsigned_magnitudes_and_memory_faults_do_not_write() {
    for (_, opcode, bits) in ABS {
        let p = decode::decode(&[0x66, 0x44, 0x0f, 0x38, opcode, 0x03], 0x1000, None).unwrap();
        let mut m = Machine::with_stack(p.entry);
        m.state.registers[3] = 0x2000;
        m.state.xmm[8] = u128::MAX;
        m.state.flags.cf = true;
        m.state.flags.of = true;
        m.state.flags.df = true;
        let mut source = 0;
        for lane in 0..128 / bits {
            source |= (1u128 << (bits - 1)) << (lane * bits);
        }
        let bytes = source.to_le_bytes();
        m.memory.map(0x2000, bytes[..8].to_vec(), false).unwrap();
        let before = m.state;
        let report = run(&p, &mut m);
        assert!(matches!(report.stop, Stop::MemoryFault(_)));
        assert_eq!(m.state, before);
        assert_eq!((report.memory_reads, report.memory_writes), (1, 0));
        assert!(!report.trace[0].completed);
        m.memory.map(0x2008, bytes[8..].to_vec(), false).unwrap();
        let report = run(&p, &mut m);
        assert_eq!(report.stop, Stop::LeftCode);
        assert_eq!((report.memory_reads, report.memory_writes), (2, 0));
        let mut expected = before;
        expected.xmm[8] = source;
        expected.rip += u64::from(p.instructions[0].len);
        assert_eq!(m.state, expected);
    }
}

#[cfg(target_arch = "x86_64")]
#[target_feature(enable = "ssse3")]
unsafe fn native_absolute(bits: u8, source: u128) -> u128 {
    use std::arch::x86_64::{__m128i, _mm_abs_epi8, _mm_abs_epi16, _mm_abs_epi32};
    let source = unsafe { std::mem::transmute::<u128, __m128i>(source) };
    let result = match bits {
        8 => _mm_abs_epi8(source),
        16 => _mm_abs_epi16(source),
        _ => _mm_abs_epi32(source),
    };
    unsafe { std::mem::transmute::<__m128i, u128>(result) }
}

#[test]
#[cfg(target_arch = "x86_64")]
fn concrete_absolute_values_match_native_for_all_bytes_words_and_dword_boundaries() {
    if !std::arch::is_x86_feature_detected!("ssse3") {
        return;
    }
    let boundaries = [
        0,
        1,
        0xffff_ffff,
        0x7fff_ffff,
        0x8000_0000,
        0x8000_0001,
        0xffff,
        0x10000,
    ];
    let mut seed = 7u64;
    for (op, opcode, bits) in ABS {
        for mode in 0..3 {
            let bytes = if mode == 2 {
                vec![0x65, 0x66, 0x47, 0x0f, 0x38, opcode, 0x44, 0x6c, 0xf0]
            } else {
                vec![
                    0x66,
                    0x45,
                    0x0f,
                    0x38,
                    opcode,
                    if mode == 1 { 0xc0 } else { 0xc1 },
                ]
            };
            let p = decode::decode(&bytes, 0x1000, None).unwrap();
            let mut m = Machine::with_stack(p.entry);
            m.state.gs_base = 0x2020;
            m.state.registers[12] = 80;
            m.state.registers[13] = 16;
            m.state.xmm.fill(u128::MAX);
            m.memory.map(0x2080, vec![0; 16], true).unwrap();
            let trials = match bits {
                8 => 256,
                16 => 65536,
                _ => 4160,
            };
            for trial in 0..trials {
                let mut source = 0;
                for lane in 0..128 / bits {
                    seed = seed.wrapping_mul(6364136223846793005).wrapping_add(1);
                    let raw = if bits < 32 {
                        (trial + u64::from(lane) * 8191) & ((1 << bits) - 1)
                    } else if trial < 64 {
                        boundaries[(trial as usize + usize::from(lane)) % 8]
                    } else {
                        seed & 0xffff_ffff
                    };
                    source |= u128::from(raw) << (lane * bits);
                }
                m.state.rip = p.entry;
                m.state.xmm[8] = if mode == 1 { source } else { !source };
                m.state.xmm[9] = source;
                m.state.flags.cf = trial & 1 != 0;
                m.state.flags.of = trial & 2 != 0;
                m.state.flags.df = trial & 4 != 0;
                m.memory.store(0x2080, 8, source as u64).unwrap();
                m.memory.store(0x2088, 8, (source >> 64) as u64).unwrap();
                let before = m.state;
                // Runtime detection above protects all SSSE3 native calls.
                let value = unsafe { native_absolute(bits, source) };
                let report = run(&p, &mut m);
                assert_eq!(report.stop, Stop::LeftCode);
                let mut expected = before;
                expected.xmm[8] = value;
                expected.rip += u64::from(p.instructions[0].len);
                assert_eq!(m.state, expected, "{op}, mode {mode}, trial {trial}");
                assert_eq!(
                    (report.memory_reads, report.memory_writes),
                    (if mode == 2 { 2 } else { 0 }, 0)
                );
                assert_eq!(m.memory.load(0x2080, 8).unwrap(), source as u64);
                assert_eq!(m.memory.load(0x2088, 8).unwrap(), (source >> 64) as u64);
            }
        }
    }
}
