use super::*;

fn k(seed: u64, tick: u64, a: u64, b: u64) -> Key { Key::new(seed, tick, Stream::Misc, a, b) }

#[test]
fn golden_values_are_frozen() {
    let got = [bits(k(0, 0, 0, 0)), bits(Key::new(42, 7, Stream::SensorNoise, 3, 9)), bits(Key::new(42, 7, Stream::SensorNoise, 3, 9).draw(1)), stagger(12345)];
    println!("{got:#x?}");
    assert_eq!(got, GOLDEN_VALUES);
}

const GOLDEN_VALUES: [u64; 4] = [0x5e59_26d3_21ba_c214, 0x7eff_3036_5716_fb71, 0x123e_1701_b8dd_b01f, 0x39a9_9518_5b53_feaa];

#[test]
fn every_field_and_swap_changes_output() {
    let base = Key::new(1, 2, Stream::FireSpread, 3, 4);
    let b = bits(base);
    let variants = [
        Key { seed: 2, ..base }, Key { tick: 3, ..base }, Key { stream: Stream::FireIgnition, ..base },
        Key { a: 4, ..base }, Key { b: 5, ..base }, base.draw(1),
        // Swapped fields.
        Key { a: 4, b: 3, ..base }, Key { tick: 3, a: 2, ..base }, Key { seed: 2, tick: 1, ..base },
    ];
    for v in variants { assert_ne!(bits(v), b, "{v:?}"); }
}

#[test]
fn uniform_and_below_ranges_and_balance() {
    let mut sum = 0.0;
    let mut counts = [0u32; 7];
    let n = 70_000u64;
    for i in 0..n {
        let u = uniform(k(9, i, 1, 2));
        assert!((0.0..1.0).contains(&u));
        sum += u;
        let x = below(k(9, i, 5, 6), 7);
        assert!(x < 7);
        counts[x as usize] += 1;
    }
    assert!((sum / n as f64 - 0.5).abs() < 0.01);
    for c in counts { assert!((c as f64 - 10_000.0).abs() < 400.0, "{counts:?}"); }
    assert_eq!(below(k(1, 1, 1, 1), 0), 0);
    assert_eq!(below(k(1, 1, 1, 1), 1), 0);
    // Large n near 2^32 still in range.
    for i in 0..1000 { assert!(below(k(3, i, 0, 0), u32::MAX - 3) < u32::MAX - 3); }
}

#[test]
fn chance_edges() {
    assert!(!chance(k(1, 1, 1, 1), 0.0));
    assert!(!chance(k(1, 1, 1, 1), -1.0));
    assert!(!chance(k(1, 1, 1, 1), f64::NAN));
    assert!(chance(k(1, 1, 1, 1), 1.0));
    assert!(chance(k(1, 1, 1, 1), 2.0));
    let hits = (0..20_000).filter(|&i| chance(k(5, i, 0, 0), 0.3)).count();
    assert!((hits as f64 / 20_000.0 - 0.3).abs() < 0.015);
}

#[test]
fn normal_moments() {
    let n = 40_000;
    let xs: Vec<f64> = (0..n).map(|i| normal(k(11, i, 0, 0).draw(0))).collect();
    assert!(xs.iter().all(|x| x.is_finite()));
    let mean = xs.iter().sum::<f64>() / n as f64;
    let var = xs.iter().map(|x| (x - mean) * (x - mean)).sum::<f64>() / n as f64;
    assert!(mean.abs() < 0.02, "{mean}");
    assert!((var - 1.0).abs() < 0.03, "{var}");
}

#[test]
fn stagger_leaves_headroom() {
    for id in 0..10_000u64 { assert!(stagger(id) < 1 << 63); }
    assert_ne!(stagger(1), stagger(2));
    assert_eq!(stagger(77), stagger(77));
}
