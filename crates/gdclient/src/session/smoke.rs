//! `-- --smoke-sortie` (release CI, after `--smoke`): start an offline skirmish in the 22 m starter, play
//! [`SORTIE_S`] seconds of sortie time, print the tick count and the error lines this run logged, and quit.
//! The packaged app holds no test drivers (export presets exclude `res://test/`), so this is how the release
//! workflow plays a sortie in the archive a player downloads.
//!
//! The run asks for 4x compression so the minute passes in about 15 s of real time. Error lines are counted
//! in this run's `user://logs/godot.log` (Godot's file logger flushes every error line at once): lines that
//! begin `ERROR:`, `SCRIPT ERROR:` or `USER ERROR:`. The process exits 0 when the sortie reached the minute
//! with no error line, 1 otherwise. Printed lines start `smoke-sortie:`.
use super::BpSession;
use crate::enums::SessionState;
use godot::prelude::*;

/// Sortie seconds the run plays.
pub(crate) const SORTIE_S: f64 = 60.0;
/// Real seconds the whole run may take before it gives up.
const LIMIT_S: f64 = 240.0;
/// Frames to wait after start-up before the skirmish is asked for, so every autoload has run its `ready`.
const SETTLE_FRAMES: u64 = 30;

/// Where the run is.
#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) enum SmokePhase { Settling, Loading, Running, Done }

#[derive(Clone, Debug)]
pub(crate) struct SmokeSortie { pub phase: SmokePhase, pub started_real_s: f64, pub start_time_s: f64 }

/// True when `--smoke-sortie` is on the command line.
pub(crate) fn wanted() -> bool { std::env::args().any(|a| a == "--smoke-sortie") }

/// Lines of a log that are errors as the release workflow counts them.
pub(crate) fn error_lines(log: &str) -> usize {
    log.lines().map(str::trim_start).filter(|l| l.starts_with("ERROR:") || l.starts_with("SCRIPT ERROR:") || l.starts_with("USER ERROR:")).count()
}

impl BpSession {
    /// One frame of the smoke run; nothing when the flag is absent.
    pub(crate) fn smoke_step(&mut self) {
        let Some(mut smoke) = self.smoke.clone() else { return };
        let now = self.run.now_s;
        if smoke.started_real_s <= 0.0 { smoke.started_real_s = now; }
        if smoke.phase != SmokePhase::Done && now - smoke.started_real_s > LIMIT_S {
            self.smoke_finish(&format!("gave up after {LIMIT_S:.0} s in {:?}", smoke.phase), false);
            smoke.phase = SmokePhase::Done;
        }
        match smoke.phase {
            SmokePhase::Settling if self.run.frame >= SETTLE_FRAMES => {
                let setup = crate::convert::d! { "blueprint" => GString::from(crate::campaign::DEFAULT_STARTER), "seed" => 7i64 };
                let problems = self.start_skirmish(setup);
                if problems.is_empty() {
                    godot_print!("smoke-sortie: skirmish loading");
                    smoke.phase = SmokePhase::Loading;
                } else {
                    self.smoke_finish(&format!("the skirmish did not start: {problems:?}"), false);
                    smoke.phase = SmokePhase::Done;
                }
            }
            SmokePhase::Loading => match self.state {
                SessionState::InSortie => {
                    smoke.start_time_s = self.time_s();
                    let _ = self.request_time_rate(4);
                    godot_print!("smoke-sortie: in sortie at {:.1} s", smoke.start_time_s);
                    smoke.phase = SmokePhase::Running;
                }
                SessionState::Failed | SessionState::Idle | SessionState::Ended => {
                    self.smoke_finish(&format!("the sortie stopped while loading ({:?})", self.state), false);
                    smoke.phase = SmokePhase::Done;
                }
                _ => {}
            },
            SmokePhase::Running => {
                if self.state != SessionState::InSortie {
                    self.smoke_finish(&format!("the sortie stopped early ({:?})", self.state), false);
                    smoke.phase = SmokePhase::Done;
                } else if self.time_s() - smoke.start_time_s >= SORTIE_S {
                    self.smoke_finish("played", true);
                    smoke.phase = SmokePhase::Done;
                }
            }
            _ => {}
        }
        self.smoke = Some(smoke);
    }

    /// Print the result and quit: exit 0 only when `played` and the log holds no error line.
    fn smoke_finish(&mut self, what: &str, played: bool) {
        let tick = self.run.newest_tick.unwrap_or(0);
        let path = godot::classes::ProjectSettings::singleton().globalize_path(crate::app::GODOT_LOG).to_string();
        let errors = std::fs::read_to_string(&path).map(|l| error_lines(&l));
        let ok = played && errors.as_ref().is_ok_and(|n| *n == 0);
        match &errors {
            Ok(n) => godot_print!("smoke-sortie: {what}; ticks {tick}; sortie time {:.1} s; error lines {n}", self.time_s()),
            Err(e) => godot_print!("smoke-sortie: {what}; ticks {tick}; sortie time {:.1} s; the log {path} could not be read: {e}", self.time_s()),
        }
        godot_print!("smoke-sortie: {}", if ok { "passed" } else { "failed" });
        self.leave(false);
        self.base().get_tree().quit_ex().exit_code(if ok { 0 } else { 1 }).done();
    }
}

#[cfg(test)]
mod tests {
    #[test]
    fn error_lines_count_engine_script_and_user_errors_only() {
        let log = "gdclient 0.1.0\nERROR: 1 resources still in use at exit\n   at: foo\nSCRIPT ERROR: Parse Error\nWARNING: soft\nUSER ERROR: x\nsmoke-sortie: error lines 0\n";
        assert_eq!(super::error_lines(log), 3);
        assert_eq!(super::error_lines(""), 0);
    }
}
