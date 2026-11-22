//! Measure the WAVs `cargo xtask gen` wrote and print one row per sound with its problems:
//!   cargo run -p audio --example report -- game/audio/gen
//! Exits 1 when a file is missing or has a problem that `analysis::problems` fails the build on.
use audio::analysis;
use audio::{Pcm16, SoundId};

fn main() {
    let dir = std::env::args().nth(1).unwrap_or_else(|| format!("game/{}", audio::GEN_DIR));
    let mut bad = 0;
    println!("{:<28} {:>6} {:>5} {:>6} {:>6} {:>6} {:>6} {:>11} {:>6}  problems", "stem", "bus", "len", "peak", "S100", "Mmax", "integ", "target", "cent");
    for id in SoundId::all() {
        let spec = id.spec();
        let path = std::path::Path::new(&dir).join(id.file_name());
        let Ok(r) = hound::WavReader::open(&path) else {
            println!("{:<28} missing {}", spec.stem, path.display());
            bad += 1;
            continue;
        };
        let ws = r.spec();
        let samples: Vec<i16> = r.into_samples::<i16>().filter_map(Result::ok).collect();
        let pcm = Pcm16 { sample_rate: ws.sample_rate, channels: ws.channels, samples, loop_frames: None };
        let rep = analysis::analyse(&pcm, spec.playback);
        let (lo, hi) = analysis::target(id);
        let mut p = analysis::problems(&spec, &rep);
        if p.is_empty() && (ws.sample_rate != audio::SAMPLE_RATE || ws.bits_per_sample != audio::BITS_PER_SAMPLE) { p.push("not 48 kHz 16-bit".into()); }
        bad += usize::from(!p.is_empty());
        p.extend(analysis::advisories(&spec, &rep).into_iter().map(|a| format!("({a})")));
        println!(
            "{:<28} {:>6} {:>5.2} {:>6.1} {:>6.1} {:>6.1} {:>6.1} {:>5.0}..{:<5.0} {:>6.0}  {}",
            spec.stem, spec.bus.name(), rep.duration_s, rep.peak_dbfs, rep.s100_max, rep.m_max, rep.integrated, lo, hi, rep.centroid_hz, p.join("; ")
        );
    }
    if bad > 0 {
        eprintln!("{bad} files with problems");
        std::process::exit(1);
    }
}
