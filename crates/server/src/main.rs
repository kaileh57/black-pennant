//! The headless binary: dedicated server, soak tests, the reference battle, the trial-battery and Fit Check
//! worker, and campaign bots. No Godot. DESIGN.md "Crate layout" (L1060-1074: "headless: dedicated server, CI
//! soak tests, Fit Check and trial-battery worker"), L1015 (dedicated server), L901 (bot matches on one hosted
//! runner over loopback UDP through the link conditioner), L1150-1168 (state hashes), L227-229 and L973 (trial
//! battery, complexity), L801-807 (Fit Check), L879-895 (campaign bots).
//!
//! Contract-owned files (no unit): this file, `error.rs`, `cmd/mod.rs`. Units: `server-args` (`args.rs`),
//! `server-runtime` (`runtime.rs`), `server-dedicated` (`cmd/serve.rs`), `server-soak` (`cmd/soak.rs`),
//! `server-reference` (`cmd/reference.rs`), `server-trials` (`cmd/trials.rs`), `server-campaign-bot`
//! (`cmd/campaign_bot.rs`).
//!
//! Exit codes: 0 success; 1 a check or gate failed (the report says which); 2 usage error; 3 runtime error.
//! CI calls `black-pennant-server reference-battle --ticks 3600` on every PR.
#![allow(dead_code, unused_variables, unused_imports)]

mod args;
mod cmd;
mod error;
mod runtime;

use error::CliError;

fn main() {
    let argv: Vec<String> = std::env::args().skip(1).collect();
    let code = match args::parse(&argv) {
        Ok(command) => match cmd::run(command) {
            Ok(code) => code,
            Err(e) => { eprintln!("error: {e}"); e.exit_code() }
        },
        Err(e) => { eprintln!("error: {e}\n\n{}", args::usage()); CliError::Usage(e).exit_code() }
    };
    std::process::exit(code);
}
