//! Unit `xt-gen-basic`. The wave shader include and the rendered audio.
//! DESIGN.md L1891 (Rust build step emits `res://shaders/gen/waves.gdshaderinc` from sim-core's packing; the
//! water, hull and VFX shaders include it), L1246 (48 kHz 16-bit WAV under `res://audio/gen/`).
//! Paths: `hull_mesh::gen_files::WAVES_INCLUDE`; `audio::GEN_DIR` + `SoundId::file_name()` for each of
//! `audio::SoundId::all()`. The include uses `sim_core::waves::MAX_WAVES` components.
//!
//! Audio renders run on scoped worker threads (one per available core, at most one per sound). Each worker
//! takes the next unrendered index from a shared counter and writes its result into that index's slot, so the
//! output list is always in `SoundId::all()` order whatever the scheduling. A render that panics becomes an
//! error naming the sound, never a partial file list.
use super::GenFile;
use std::path::Path;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Mutex;

/// `shaders/gen/waves.gdshaderinc` from `sim_core::waves::emit_wave_shader_include(MAX_WAVES)`. Unit: `xt-gen-basic`.
pub fn waves(root: &Path) -> Result<Vec<GenFile>, String> {
    let text = sim_core::waves::emit_wave_shader_include(sim_core::waves::MAX_WAVES);
    if text.is_empty() { return Err("the wave include emitter returned no text".into()); }
    Ok(vec![GenFile { path: hull_mesh::gen_files::WAVES_INCLUDE.to_string(), bytes: text.into_bytes() }])
}

/// One WAV per sound via `audio::render_wav`, in `SoundId::all()` order. Renders may run on a thread pool as
/// long as the output list is assembled in that fixed order. Unit: `xt-gen-basic`.
pub fn audio(root: &Path) -> Result<Vec<GenFile>, String> {
    let ids = audio::SoundId::all();
    let paths: Vec<String> = ids.iter().map(|id| format!("{}/{}", audio::GEN_DIR, id.file_name())).collect();
    for (i, p) in paths.iter().enumerate() {
        if paths[..i].contains(p) { return Err(format!("two sounds write {p}")); }
    }
    let wavs = map_in_order(&ids, workers(), |&id| audio::render_wav(id)).map_err(|i| format!("rendering {} panicked", paths[i]))?;
    Ok(paths.into_iter().zip(wavs).map(|(path, bytes)| GenFile { path, bytes }).collect())
}

pub(crate) fn workers() -> usize { std::thread::available_parallelism().map_or(4, |n| n.get()).clamp(1, 16) }

/// `f` over `items` on up to `threads` scoped threads; results come back in `items` order. `Err(i)` names the
/// first item (in order) whose call panicked.
pub(crate) fn map_in_order<T: Sync, R: Send>(items: &[T], threads: usize, f: impl Fn(&T) -> R + Sync) -> Result<Vec<R>, usize> {
    let slots: Vec<Mutex<Option<std::thread::Result<R>>>> = items.iter().map(|_| Mutex::new(None)).collect();
    let next = AtomicUsize::new(0);
    let threads = threads.clamp(1, items.len().max(1));
    std::thread::scope(|s| {
        for _ in 0..threads {
            s.spawn(|| loop {
                let i = next.fetch_add(1, Ordering::Relaxed);
                let Some(item) = items.get(i) else { break };
                let r = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| f(item)));
                if let Ok(mut slot) = slots[i].lock() { *slot = Some(r); }
            });
        }
    });
    let mut out = Vec::with_capacity(items.len());
    for (i, slot) in slots.into_iter().enumerate() {
        match slot.into_inner().ok().flatten() {
            Some(Ok(r)) => out.push(r),
            _ => return Err(i),
        }
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn wave_include_is_written_where_the_shaders_include_it() {
        let files = waves(Path::new(".")).unwrap();
        assert_eq!(files.len(), 1);
        assert_eq!(files[0].path, "shaders/gen/waves.gdshaderinc");
        let text = String::from_utf8(files[0].bytes.clone()).unwrap();
        for name in sim_core::waves::WAVE_MAT_UNIFORMS { assert!(text.contains(name), "{name} missing"); }
        assert!(text.contains(&format!("BP_WAVES_COUNT = {}", sim_core::waves::MAX_WAVES)));
        assert_eq!(files, waves(Path::new(".")).unwrap());
    }

    #[test]
    fn map_keeps_order_whatever_the_thread_count() {
        let items: Vec<u64> = (0..97).collect();
        let slow = |&x: &u64| { if x % 7 == 0 { std::thread::sleep(std::time::Duration::from_millis(2)); } x * x + 1 };
        let serial: Vec<u64> = items.iter().map(slow).collect();
        for threads in [1, 3, 16, 200] { assert_eq!(map_in_order(&items, threads, slow).unwrap(), serial); }
        assert_eq!(map_in_order(&[] as &[u64], 4, slow).unwrap(), Vec::<u64>::new());
    }

    #[test]
    fn a_panicking_render_is_an_error_not_a_gap() {
        let items: Vec<u32> = (0..10).collect();
        let r = map_in_order(&items, 4, |&x| if x == 6 { panic!("expected test panic") } else { x });
        assert_eq!(r, Err(6));
    }

    #[test]
    fn every_sound_has_its_own_generated_path() {
        let ids = audio::SoundId::all();
        let mut paths: Vec<String> = ids.iter().map(|id| format!("{}/{}", audio::GEN_DIR, id.file_name())).collect();
        assert!(paths.iter().all(|p| p.starts_with("audio/gen/") && p.ends_with(".wav")));
        let n = paths.len();
        paths.sort();
        paths.dedup();
        assert_eq!(paths.len(), n);
    }
}
