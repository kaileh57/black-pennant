//! Contract-owned (no work unit). CLI errors and exit codes. Developer-facing English is fine here: the binary
//! has no players' UI and its output goes to CI logs and terminals.
use crate::args::ArgError;

#[derive(Debug)]
pub enum CliError {
    Usage(ArgError),
    Io(std::io::Error),
    /// RON config or input file could not be read or parsed.
    Config(String),
    /// Embedded data failed to load (`content::ContentError`).
    Content(String),
    Net(net::NetError),
    Sim(sim_systems::SimError),
}

impl std::fmt::Display for CliError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            CliError::Usage(e) => write!(f, "{e}"),
            CliError::Io(e) => write!(f, "io: {e}"),
            CliError::Config(e) => write!(f, "config: {e}"),
            CliError::Content(e) => write!(f, "content: {e}"),
            CliError::Net(e) => write!(f, "net: {e}"),
            CliError::Sim(e) => write!(f, "simulation: {e}"),
        }
    }
}

impl CliError {
    pub fn exit_code(&self) -> i32 { match self { CliError::Usage(_) => 2, _ => 3 } }
}

impl From<std::io::Error> for CliError { fn from(e: std::io::Error) -> Self { CliError::Io(e) } }
impl From<net::NetError> for CliError { fn from(e: net::NetError) -> Self { CliError::Net(e) } }
impl From<sim_systems::SimError> for CliError { fn from(e: sim_systems::SimError) -> Self { CliError::Sim(e) } }

/// Exit code of a finished command: 0 pass, 1 a check or gate failed.
pub type Code = i32;
pub const PASS: Code = 0;
pub const FAIL: Code = 1;
