//! Every sound the game ships, rendered to WAV bytes exactly as `cargo xtask gen` writes them, read back and
//! measured (`audio::analysis`): level against its bus target, clipping, DC, silence, length, loop seams,
//! spectral balance and harshness, and how alike sounds of one family are. Prints the table
//! (`cargo test -p audio --test levels -- --nocapture`) and fails on any problem.
use audio::analysis::{self, Report};
use audio::{Pcm16, Playback, SoundId};

fn decode(bytes: &[u8]) -> Pcm16 {
    let r = hound::WavReader::new(std::io::Cursor::new(bytes)).expect("a WAV");
    let spec = r.spec();
    assert_eq!((spec.sample_rate, spec.bits_per_sample), (48_000, 16));
    let samples: Vec<i16> = r.into_samples::<i16>().map(|s| s.unwrap()).collect();
    Pcm16 { sample_rate: spec.sample_rate, channels: spec.channels, samples, loop_frames: None }
}

fn render_all() -> Vec<(SoundId, Report)> {
    let ids = SoundId::all();
    let threads = std::thread::available_parallelism().map_or(4, |n| n.get()).min(12);
    let chunks: Vec<Vec<SoundId>> = (0..threads).map(|t| ids.iter().copied().skip(t).step_by(threads).collect()).collect();
    let mut out: Vec<(SoundId, Report)> = std::thread::scope(|s| {
        let hs: Vec<_> = chunks.iter().map(|c| s.spawn(move || c.iter().map(|&id| (id, analysis::analyse(&decode(&audio::render_wav(id)), id.spec().playback))).collect::<Vec<_>>())).collect();
        hs.into_iter().flat_map(|h| h.join().unwrap()).collect()
    });
    out.sort_by_key(|(id, _)| ids.iter().position(|x| x == id));
    out
}

fn family(id: SoundId) -> u8 {
    match id { SoundId::Weapon(_) => 0, SoundId::Impact(_) => 1, SoundId::Ambience(_) => 2, SoundId::Alert(_) => 3, SoundId::Radio(_) => 4, SoundId::Ui(_) => 5, SoundId::MenuTheme => 6 }
}

#[test]
fn every_sound_is_clean_and_levelled() {
    let all = render_all();
    println!("{:<28} {:>4} {:>5} {:>6} {:>6} {:>6} {:>6} {:>6} {:>11} {:>5} {:>5} {:>5} {:>6} {:>5}  {:<24} {}", "stem", "bus", "len", "peak", "rms", "S100", "Mmax", "integ", "target", "aud", "tail", "small", "cent", "harsh", "sub/low/mid/pres/air %", "problems");
    let mut failures = Vec::new();
    for (id, r) in &all {
        let spec = id.spec();
        let (lo, hi) = analysis::target(*id);
        let pr = analysis::problems(&spec, r);
        let adv = analysis::advisories(&spec, r);
        let b = r.bands.map(|x| (x * 100.0).round() as i32);
        let extra = if spec.playback == Playback::Loop { format!(" seam {:.1}x {:+.1}LU", r.seam_step, r.seam_lu) } else { String::new() };
        println!(
            "{:<28} {:>4} {:>5.2} {:>6.1} {:>6.1} {:>6.1} {:>6.1} {:>6.1} {:>5.0}..{:<5.0} {:>5.2} {:>5.0} {:>5.0} {:>6.0} {:>5.0}  {:<24} {}{}",
            spec.stem, &spec.bus.name()[..spec.bus.name().len().min(4)], r.duration_s, r.peak_dbfs, r.rms_dbfs, r.s100_max, r.m_max, r.integrated, lo, hi, r.audible_s, r.tail_lu, r.small_lu, r.centroid_hz, r.harsh * 100.0,
            format!("{}/{}/{}/{}/{}", b[0], b[1], b[2], b[3], b[4]), pr.iter().chain(adv.iter().map(|a| a)).map(|x| x.as_str()).collect::<Vec<_>>().join("; "), extra
        );
        if !pr.is_empty() { failures.push(format!("{}: {}", spec.stem, pr.join("; "))); }
    }
    // Too alike: the closest pairs within a family.
    let mut pairs = Vec::new();
    for (i, (a, ra)) in all.iter().enumerate() {
        for (b, rb) in all.iter().skip(i + 1) {
            if family(*a) == family(*b) { pairs.push((analysis::similarity(ra, rb), *a, *b)); }
        }
    }
    pairs.sort_by(|x, y| y.0.total_cmp(&x.0));
    println!("\nmost alike within a family:");
    for (s, a, b) in pairs.iter().take(12) { println!("  {:.3}  {} ~ {}", s, a.spec().stem, b.spec().stem); }
    // Pairs that are meant to be variants of one sound (near and far, calm and rough) may be close; any other
    // pair this alike is a copy.
    let allowed = |a: SoundId, b: SoundId| {
        use audio::{AmbienceSfx as A, RadioSfx as R};
        matches!((a, b),
            (SoundId::Ambience(A::SeaCalm | A::SeaModerate | A::SeaRough | A::SeaStorm), SoundId::Ambience(A::SeaModerate | A::SeaRough | A::SeaStorm))
            | (SoundId::Ambience(A::HullWashSlow), SoundId::Ambience(A::HullWashFast))
            | (SoundId::Ambience(A::Wind), SoundId::Ambience(A::WindGale))
            | (SoundId::Ambience(A::Rain), SoundId::Ambience(A::RainHeavy))
            | (SoundId::Ambience(A::SonarPing), SoundId::Ambience(A::SonarPingHeard))
            | (SoundId::Radio(R::SquelchOpen), SoundId::Radio(R::AceOpen))
            // One gun heard near and far.
            | (SoundId::Weapon(audio::WeaponSfx::GunLarge | audio::WeaponSfx::GunHeavy), SoundId::Weapon(audio::WeaponSfx::GunDistantHeavy))
            | (SoundId::Weapon(audio::WeaponSfx::GunAuto | audio::WeaponSfx::GunMedium), SoundId::Weapon(audio::WeaponSfx::GunDistantLight))
            // Mirror images: the same two notes rising and falling.
            | (SoundId::Ui(audio::UiSfx::Pause), SoundId::Ui(audio::UiSfx::Resume)))
    };
    for (s, a, b) in &pairs {
        if *s > 0.97 && !allowed(*a, *b) { failures.push(format!("{} and {} are too alike ({s:.3})", a.spec().stem, b.spec().stem)); }
    }
    // Alert priorities are heard in order of urgency, judged on 100 ms blocks (the ear's window for a tick),
    // with at least 1 LU between neighbours; every alert is over every radio cue and every radio cue over every
    // interface sound.
    let s100 = |id: SoundId| all.iter().find(|(x, _)| *x == id).map(|(_, r)| r.s100_max).unwrap();
    use audio::AlertSound as P;
    let order = [P::Emergency, P::Alarm, P::Warning, P::Caution].map(|p| s100(SoundId::Alert(p)));
    println!("alert 100 ms levels, Emergency to Caution: {order:?}");
    if !(order[0] >= order[2] + 1.0 && order[1] >= order[2] + 1.0 && order[2] >= order[3] + 1.0) {
        failures.push(format!("alert priorities not in order of level: {order:?}"));
    }
    let bus_max = |b: audio::Bus, skip: Option<SoundId>| all.iter().filter(|(id, _)| id.spec().bus == b && Some(*id) != skip).map(|(_, r)| r.s100_max).fold(-200.0f32, f32::max);
    let alert_min = order.iter().copied().fold(0.0f32, f32::min);
    let radio_max = bus_max(audio::Bus::Radio, None);
    let ui_max = bus_max(audio::Bus::Ui, None);
    println!("100 ms levels: quietest alert {alert_min:.1}, loudest radio cue {radio_max:.1}, loudest interface sound {ui_max:.1}");
    if !(alert_min >= radio_max + 1.0 && radio_max >= ui_max + 1.0) {
        failures.push(format!("buses not in order: quietest alert {alert_min:.1}, loudest radio {radio_max:.1}, loudest interface {ui_max:.1}"));
    }
    assert!(failures.is_empty(), "{} problems:\n{}", failures.len(), failures.join("\n"));
}

/// Power sum of loudness values, LUFS.
fn sum_lufs(levels: &[f32]) -> f32 {
    let p: f64 = levels.iter().filter(|l| l.is_finite() && **l > -150.0).map(|&l| libm::pow(10.0, l as f64 / 10.0)).sum();
    if p > 0.0 { (10.0 * libm::log10(p)) as f32 } else { -200.0 }
}

/// The mix as heard from the bridge camera (about 60 m from the engines): the Ambience bus summed for a few
/// sea, wind, rain and speed states through `mix`, against gun reports at 1, 5 and 10 km and the own gun.
/// A report at range is the cue a captain listens for: it must stay near or above the bed it plays over.
#[test]
fn ambience_bus_leaves_room_for_reports_at_range() {
    use audio::mix;
    use audio::{AmbienceSfx as A, WeaponSfx as W};
    let lufs = |id: SoundId| {
        let r = analysis::analyse(&audio::render(id), id.spec().playback);
        analysis::judged_loudness(&id.spec(), &r)
    };
    let beds: Vec<(A, f32)> = [A::SeaCalm, A::SeaModerate, A::SeaRough, A::SeaStorm, A::Wind, A::WindGale, A::Rain, A::RainHeavy, A::EngineDiesel, A::HullWashSlow, A::HullWashFast, A::Cavitation]
        .into_iter()
        .map(|a| (a, lufs(SoundId::Ambience(a))))
        .collect();
    let l = |a: A| beds.iter().find(|(x, _)| *x == a).unwrap().1;
    let own_gun = lufs(SoundId::Weapon(W::GunMedium)) + mix::distance_gain_db(30.0);
    let near_1km = lufs(SoundId::Weapon(W::GunMedium)) + mix::distance_gain_db(1_000.0);
    let far = lufs(SoundId::Weapon(W::GunDistantHeavy));
    let far_5km = far + mix::gun_report_trim_db(127, 5_000.0) + mix::distance_gain_db(5_000.0);
    let far_10km = far + mix::gun_report_trim_db(127, 10_000.0) + mix::distance_gain_db(10_000.0);
    let light_5km = lufs(SoundId::Weapon(W::GunDistantLight)) + mix::gun_report_trim_db(76, 5_000.0) + mix::distance_gain_db(5_000.0);
    println!("own 76 mm gun {own_gun:.1}, 76 mm at 1 km {near_1km:.1}, 127 mm at 5 km {far_5km:.1}, at 10 km {far_10km:.1}, 76 mm at 5 km {light_5km:.1} LUFS");
    // Crossing the near-report range never makes a gun louder.
    for (cal, near_file) in [(30u16, W::GunAuto), (76, W::GunMedium), (127, W::GunLarge), (155, W::GunHeavy)] {
        let distant_file = if cal <= 88 { W::GunDistantLight } else { W::GunDistantHeavy };
        let inside = lufs(SoundId::Weapon(near_file)) + mix::distance_gain_db(1_400.0);
        let outside = lufs(SoundId::Weapon(distant_file)) + mix::gun_report_trim_db(cal, 1_600.0) + mix::distance_gain_db(1_600.0);
        println!("{cal} mm at 1.4 km {inside:.1}, at 1.6 km {outside:.1}");
        assert!(outside <= inside + 0.5, "{cal} mm gets louder past {} m: {inside:.1} -> {outside:.1}", mix::NEAR_REPORT_M);
    }
    // (sea state, wind m/s, rain, speed kn, shaft power)
    let states = [(1u8, 2.0f32, 0.0f32, 8.0f32, 0.2f32), (4, 9.0, 0.0, 12.0, 0.1), (4, 9.0, 0.0, 28.0, 0.8), (6, 15.0, 0.5, 20.0, 0.5), (8, 25.0, 1.0, 30.0, 1.0)];
    let mut failures = Vec::new();
    for (sea, wind, rain, kn, power) in states {
        let s = mix::sea_params(sea, wind);
        let w = mix::weather_params(wind, rain);
        let wash = mix::wash_params(kn, 20.0);
        let eng = mix::engine_params(mix::EngineClass::Diesel, power);
        let parts = [
            l(A::SeaCalm) + s[0].gain_db, l(A::SeaModerate) + s[1].gain_db, l(A::SeaRough) + s[2].gain_db, l(A::SeaStorm) + s[3].gain_db,
            l(A::Wind) + w[0].gain_db, l(A::WindGale) + w[1].gain_db, l(A::Rain) + w[2].gain_db, l(A::RainHeavy) + w[3].gain_db,
            l(A::HullWashSlow) + wash[0].gain_db, l(A::HullWashFast) + wash[1].gain_db, l(A::Cavitation) + wash[2].gain_db,
            l(A::EngineDiesel) + eng.gain_db + mix::distance_gain_db(60.0),
        ];
        let bed = sum_lufs(&parts);
        println!("sea {sea} wind {wind} rain {rain} {kn} kn power {power}: ambience bus {bed:.1} LUFS; parts {:?}", parts.map(|p| (p * 10.0).round() / 10.0));
        // Margins (each test value clears its limit by at least 1 LU, so platform rounding cannot flip it): the
        // own gun stands 10 LU over the bed (7 in a storm), a gun at 1 km is within 1 LU of it, a 127 mm report
        // at 5 km at most 7 LU under it (short of a gale), and calm water leaves the bus under -28 LUFS.
        let own_margin = if sea >= 8 { 7.0 } else { 10.0 };
        if own_gun < bed + own_margin { failures.push(format!("sea {sea}: own gun {own_gun:.1} not {own_margin} LU over the bed {bed:.1}")); }
        if sea <= 6 && near_1km < bed - 1.0 { failures.push(format!("sea {sea}: a gun at 1 km {near_1km:.1} more than 1 LU under the bed {bed:.1}")); }
        if sea <= 6 && far_5km < bed - 7.0 { failures.push(format!("sea {sea}: a report at 5 km {far_5km:.1} more than 7 LU under the bed {bed:.1}")); }
        if sea <= 4 && kn < 15.0 && bed > -28.0 { failures.push(format!("sea {sea}: the cruising bed {bed:.1} is louder than -28 LUFS")); }
    }
    assert!(failures.is_empty(), "{}", failures.join("\n"));
}
