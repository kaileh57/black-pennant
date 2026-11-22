//! Contract-owned (no work unit). Dispatch to the subcommand modules.
pub mod campaign_bot;
pub mod reference;
pub mod serve;
pub mod soak;
pub mod trials;

use crate::args::{usage, Command};
use crate::error::{CliError, Code, PASS};

pub fn run(command: Command) -> Result<Code, CliError> {
    match command {
        Command::Help => { println!("{}", usage()); Ok(PASS) }
        Command::Version => { println!("black-pennant-server {} (protocol {})", env!("CARGO_PKG_VERSION"), net::PROTOCOL_VERSION); Ok(PASS) }
        Command::Serve(a) => serve::run(&a),
        Command::Soak(a) => soak::run(&a),
        Command::Reference(a) => reference::run(&a),
        Command::Trials(a) => trials::run_trials(&a),
        Command::FitCheck(a) => trials::run_fit_check(&a),
        Command::CampaignBot(a) => campaign_bot::run(&a),
    }
}
