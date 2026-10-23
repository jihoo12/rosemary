use rosemary::{
    decode,
    dynamic::{self, Machine, Options, Stop},
    ir::Program,
};

const OPS: [(&str, u8, u8); 6] = [
    ("phaddw", 1, 16),
    ("phaddd", 2, 32),
    ("phaddsw", 3, 16),
    ("phsubw", 5, 16),
    ("phsubd", 6, 32),
    ("phsubsw", 7, 16),
];

fn vector(bits: u8, values: &[i64]) -> u128 {
    let mask = (1u64 << bits) - 1;
    values.iter().enumerate().fold(0, |v, (n, value)| {
        v | (u128::from(*value as u64 & mask) << (n * usize::from(bits)))
    })
}

fn run(p: &Program, m: &mut Machine) -> dynamic::Report {
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
fn horizontal_lane_order_wrap_and_saturation_are_distinct() {
    for (_, opcode, bits) in OPS {
        let (left, right, expected) = match opcode {
            1 => (
                vector(16, &[32767, 1, -32768, -1, 100, -200, -300, 400]),
                vector(16, &[-32768, 32767, 32767, 32767, 32767, -32768, 300, -400]),
                vector(16, &[-32768, 32767, -100, 100, -1, -2, -1, -100]),
            ),
            3 => (
                vector(16, &[32767, 1, -32768, -1, 100, -200, -300, 400]),
                vector(16, &[-32768, 32767, 32767, 32767, 32767, -32768, 300, -400]),
                vector(16, &[32767, -32768, -100, 100, -1, 32767, -1, -100]),
            ),
            5 => (
                vector(16, &[32767, 1, -32768, -1, 100, -200, -300, 400]),
                vector(16, &[-32768, 32767, 32767, 32767, 32767, -32768, 300, -400]),
                vector(16, &[32766, -32767, 300, -700, 1, 0, -1, 700]),
            ),
            7 => (
                vector(16, &[32767, 1, -32768, -1, 100, -200, -300, 400]),
                vector(16, &[-32768, 32767, 32767, 32767, 32767, -32768, 300, -400]),
                vector(16, &[32766, -32767, 300, -700, -32768, 0, 32767, 700]),
            ),
            2 => (
                vector(32, &[2147483647, 1, -2147483648, -1]),
                vector(32, &[100, -200, -300, 400]),
                vector(32, &[-2147483648, 2147483647, -100, 100]),
            ),
            _ => (
                vector(32, &[2147483647, 1, -2147483648, -1]),
                vector(32, &[100, -200, -300, 400]),
                vector(32, &[2147483646, -2147483647, 300, -700]),
            ),
        };
        let p = decode::decode(&[0x66, 0x0f, 0x38, opcode, 0xc1], 0x1000, None).unwrap();
        let mut m = Machine::with_stack(p.entry);
        m.state.xmm[0] = left;
        m.state.xmm[1] = right;
        m.state.flags.cf = true;
        m.state.flags.of = true;
        m.state.flags.df = true;
        let before = m.state;
        assert_eq!(run(&p, &mut m).stop, Stop::LeftCode);
        let mut after = before;
        after.xmm[0] = expected;
        after.rip += u64::from(p.instructions[0].len);
        assert_eq!(m.state, after, "opcode {opcode}, width {bits}");
    }
}

#[test]
fn horizontal_memory_faults_preserve_all_destination_lanes() {
    for (_, opcode, _) in OPS {
        let p = decode::decode(&[0x66, 0x44, 0x0f, 0x38, opcode, 0x03], 0x1000, None).unwrap();
        let mut m = Machine::with_stack(p.entry);
        m.state.registers[3] = 0x2000;
        m.state.xmm[8] = u128::MAX;
        m.memory.map(0x2000, vec![0; 8], false).unwrap();
        let before = m.state;
        let report = run(&p, &mut m);
        assert!(matches!(report.stop, Stop::MemoryFault(_)));
        assert_eq!(m.state, before);
        assert_eq!((report.memory_reads, report.memory_writes), (1, 0));
        assert!(!report.trace[0].completed);
        m.memory.map(0x2008, vec![0; 8], false).unwrap();
        let report = run(&p, &mut m);
        assert_eq!(report.stop, Stop::LeftCode);
        assert_eq!((report.memory_reads, report.memory_writes), (2, 0));
        assert_eq!(m.state.flags, before.flags);
        assert_eq!(m.state.registers, before.registers);
    }
}

#[cfg(target_arch = "x86_64")]
#[target_feature(enable = "ssse3")]
unsafe fn native_horizontal(opcode: u8, left: u128, right: u128) -> u128 {
    use std::arch::x86_64::{
        __m128i, _mm_hadd_epi16, _mm_hadd_epi32, _mm_hadds_epi16, _mm_hsub_epi16, _mm_hsub_epi32,
        _mm_hsubs_epi16,
    };
    let left = unsafe { std::mem::transmute::<u128, __m128i>(left) };
    let right = unsafe { std::mem::transmute::<u128, __m128i>(right) };
    let result = match opcode {
        1 => _mm_hadd_epi16(left, right),
        2 => _mm_hadd_epi32(left, right),
        3 => _mm_hadds_epi16(left, right),
        5 => _mm_hsub_epi16(left, right),
        6 => _mm_hsub_epi32(left, right),
        _ => _mm_hsubs_epi16(left, right),
    };
    unsafe { std::mem::transmute::<__m128i, u128>(result) }
}

#[test]
#[cfg(target_arch = "x86_64")]
fn concrete_horizontal_pairs_match_native_for_all_words_and_dword_boundaries() {
    if !std::arch::is_x86_feature_detected!("ssse3") {
        return;
    }
    let partners = [0, 1, 0xffff, 0x8000, 0x7fff, 0x8001, 0x4000, 0xc000];
    let boundaries = [
        0,
        1,
        0xffff_ffff,
        0x7fff_ffff,
        0x8000_0000,
        0x8000_0001,
        0x4000_0000,
        0xc000_0000,
    ];
    let mut seed = 73u64;
    for (op, opcode, bits) in OPS {
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
            let trials = if bits == 16 { 65536 } else { 4160 };
            for trial in 0..trials {
                let mut left = 0;
                let mut right = 0;
                for lane in 0..128 / bits {
                    seed = seed.wrapping_mul(6364136223846793005).wrapping_add(1);
                    let a = if bits == 16 {
                        if lane % 2 == 0 {
                            trial
                        } else {
                            partners[usize::from(lane) / 2]
                        }
                    } else if trial < 64 {
                        boundaries[(trial as usize / 8 + usize::from(lane)) % 8]
                    } else {
                        seed & 0xffff_ffff
                    };
                    seed = seed.wrapping_mul(6364136223846793005).wrapping_add(1);
                    let b = if bits == 16 {
                        if lane % 2 == 0 {
                            65535 - trial
                        } else {
                            partners[4 + usize::from(lane) / 2]
                        }
                    } else if trial < 64 {
                        boundaries[(trial as usize % 8 + usize::from(lane)) % 8]
                    } else {
                        seed & 0xffff_ffff
                    };
                    left |= u128::from(a) << (lane * bits);
                    right |= u128::from(b) << (lane * bits);
                }
                if mode == 1 {
                    right = left;
                }
                m.state.rip = p.entry;
                m.state.xmm[8] = left;
                m.state.xmm[9] = right;
                m.state.flags.cf = trial & 1 != 0;
                m.state.flags.pf = trial & 2 != 0;
                m.state.flags.zf = trial & 4 != 0;
                m.state.flags.sf = trial & 8 != 0;
                m.state.flags.of = trial & 16 != 0;
                m.state.flags.df = trial & 32 != 0;
                m.memory.store(0x2080, 8, right as u64).unwrap();
                m.memory.store(0x2088, 8, (right >> 64) as u64).unwrap();
                let before = m.state;
                // Runtime feature detection above protects this SSSE3 call.
                let expected = unsafe { native_horizontal(opcode, left, right) };
                let report = run(&p, &mut m);
                assert_eq!(report.stop, Stop::LeftCode);
                let mut after = before;
                after.xmm[8] = expected;
                after.rip += u64::from(p.instructions[0].len);
                assert_eq!(m.state, after, "{op}, mode {mode}, trial {trial}");
                assert_eq!(
                    (report.memory_reads, report.memory_writes),
                    (if mode == 2 { 2 } else { 0 }, 0)
                );
                assert_eq!(m.memory.load(0x2080, 8).unwrap(), right as u64);
                assert_eq!(m.memory.load(0x2088, 8).unwrap(), (right >> 64) as u64);
            }
        }
    }
}
