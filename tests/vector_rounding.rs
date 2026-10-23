use rosemary::{
    decode,
    dynamic::{self, Machine, Options, Stop},
};

fn words(values: [i16; 8]) -> u128 {
    values.iter().enumerate().fold(0, |v, (n, value)| {
        v | (u128::from(*value as u16) << (n * 16))
    })
}

#[test]
fn rounded_products_distinguish_halfway_values_and_wrapping_overflow() {
    let p = decode::decode(&[0x66, 0x0f, 0x38, 0x0b, 0xc1], 0x1000, None).unwrap();
    let mut m = Machine::with_stack(p.entry);
    m.state.xmm[0] = words([1, -1, 3, -3, 1, -1, -32768, 32767]);
    m.state.xmm[1] = words([16384, 16384, 16384, 16384, 16383, 16385, -32768, 32767]);
    m.state.flags.cf = true;
    m.state.flags.of = true;
    m.state.flags.df = true;
    m.state.mxcsr = 0x5fc0;
    let before = m.state;
    let report = dynamic::execute(&p, &mut m, Options::default()).unwrap();
    assert_eq!(report.stop, Stop::LeftCode);
    let mut expected = before;
    expected.xmm[0] = words([1, 0, 2, -1, 0, -1, -32768, 32766]);
    expected.rip += u64::from(p.instructions[0].len);
    assert_eq!(m.state, expected);
}

#[test]
fn zero_multipliers_still_read_every_memory_lane_and_fault_atomically() {
    let p = decode::decode(&[0x66, 0x44, 0x0f, 0x38, 0x0b, 0x03], 0x1000, None).unwrap();
    let mut m = Machine::with_stack(p.entry);
    m.state.registers[3] = 0x2000;
    m.memory.map(0x2000, vec![255; 8], false).unwrap();
    let before = m.state;
    let report = dynamic::execute(&p, &mut m, Options::default()).unwrap();
    assert!(matches!(report.stop, Stop::MemoryFault(_)));
    assert_eq!(m.state, before);
    assert_eq!((report.memory_reads, report.memory_writes), (1, 0));
    assert!(!report.trace[0].completed);
    m.memory.map(0x2008, vec![255; 8], false).unwrap();
    let report = dynamic::execute(&p, &mut m, Options::default()).unwrap();
    assert_eq!(report.stop, Stop::LeftCode);
    assert_eq!((report.memory_reads, report.memory_writes), (2, 0));
    let mut expected = before;
    expected.rip += u64::from(p.instructions[0].len);
    assert_eq!(m.state, expected);
}

#[cfg(target_arch = "x86_64")]
#[target_feature(enable = "ssse3")]
unsafe fn native_round(left: u128, right: u128) -> u128 {
    use std::arch::x86_64::{__m128i, _mm_mulhrs_epi16};
    let left = unsafe { std::mem::transmute::<u128, __m128i>(left) };
    let right = unsafe { std::mem::transmute::<u128, __m128i>(right) };
    unsafe { std::mem::transmute::<__m128i, u128>(_mm_mulhrs_epi16(left, right)) }
}

#[test]
#[cfg(target_arch = "x86_64")]
fn concrete_rounded_products_match_native_for_every_word_and_rounding_boundaries() {
    if !std::arch::is_x86_feature_detected!("ssse3") {
        return;
    }
    let factors = [0, 1, 0xffff, 0x7fff, 0x8000, 0x8001, 0x4000, 0xc000];
    let mut seed = 73u64;
    for mode in 0..3 {
        let bytes = if mode == 2 {
            vec![0x65, 0x66, 0x47, 0x0f, 0x38, 0x0b, 0x44, 0x6c, 0xf0]
        } else {
            vec![
                0x66,
                0x45,
                0x0f,
                0x38,
                0x0b,
                if mode == 1 { 0xc0 } else { 0xc1 },
            ]
        };
        let p = decode::decode(&bytes, 0x1000, None).unwrap();
        let mut m = Machine::with_stack(p.entry);
        m.state.gs_base = 0x2020;
        m.state.registers[12] = 80;
        m.state.registers[13] = 16;
        m.state.xmm.fill(u128::MAX);
        m.state.mxcsr = 0x5fc0;
        m.memory.map(0x2080, vec![0; 16], true).unwrap();
        let trials = if mode == 1 { 65536 } else { 131072 };
        for trial in 0..trials {
            let mut left = 0;
            let mut right = 0;
            for (lane, factor) in factors.iter().enumerate() {
                seed = seed.wrapping_mul(6364136223846793005).wrapping_add(1);
                let a = (trial + lane as u64 * 8191) & 65535;
                let b = if trial < 65536 {
                    *factor
                } else {
                    (seed >> 16) & 65535
                };
                left |= u128::from(a) << (lane * 16);
                right |= u128::from(b) << (lane * 16);
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
            // Runtime SSSE3 detection above protects the native instruction.
            let value = unsafe { native_round(left, right) };
            let report = dynamic::execute(
                &p,
                &mut m,
                Options {
                    max_steps: 1,
                    trace_limit: 0,
                },
            )
            .unwrap();
            assert_eq!(report.stop, Stop::LeftCode);
            let mut expected = before;
            expected.xmm[8] = value;
            expected.rip += u64::from(p.instructions[0].len);
            assert_eq!(m.state, expected, "mode {mode}, trial {trial}");
            assert_eq!(
                (report.memory_reads, report.memory_writes),
                (if mode == 2 { 2 } else { 0 }, 0)
            );
            assert_eq!(m.memory.load(0x2080, 8).unwrap(), right as u64);
            assert_eq!(m.memory.load(0x2088, 8).unwrap(), (right >> 64) as u64);
        }
    }
}
