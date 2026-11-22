//! Unit `server-args`. Hand-rolled argument parsing (no clap): `black-pennant-server <command> [flags]`.
//! Flags are `--name value` or `--name=value`; booleans are bare `--name`. Unknown flags and commands are
//! errors. `help`, `--help` and `-h` anywhere print [`usage`]; `version` and `--version` print the version.
//!
//! | Command | Flags (defaults) |
//! | `serve` | `--config PATH` (dedicated RON config, `content::settings::ServerConfig`), `--port N` (overrides config; 47610), `--no-map` (skip port mapping), `--log PATH` (connection log file; default `connection.log` beside the config) |
//! | `soak` | `--minutes F` (10), `--clients N` (2), `--link clean|gate|tolerated|drop-ping|drop-loss|upload-1mbps` (clean), `--scene reference|coop|swarm` (reference), `--seed N` (1), `--udp` (loopback UDP; default in-memory), `--synthetic-tick-ms F` (0), `--out PATH` (RON report) |
//! | `reference-battle` | `--scene reference|coop|swarm` (reference), `--seed N` (1), `--ticks N` (3600), `--subsystems` (also print per-subsystem hashes), `--threads N` (3), `--cpu-clock` (time by the thread's CPU clock; needs `--threads 1`) |
//! | `trials` | `--blueprint PATH` (required), `--spec on-save|ci|short` (ci), `--out PATH` |
//! | `fit-check` | `--blueprint PATH` (required), `--brief PATH` (required; RON `FitBrief`), `--seconds F` (30), `--out PATH` |
//! | `campaign-bot` | `--policy aggressive|careful|mixed|all-ghost|no-ghost|single:<method>` (mixed), `--seed N` (1), `--runs N` (1), `--until-act 1..4` (1), `--players N` (1), `--resolver full|abstract` (full), `--shard I/N` (0/1), `--max-hours N` (10000), `--out PATH` |
//! | `help`, `version` | |
use std::path::PathBuf;

#[derive(Clone, Debug, PartialEq)]
pub enum Command { Help, Version, Serve(ServeArgs), Soak(SoakArgs), Reference(ReferenceArgs), Trials(TrialsArgs), FitCheck(FitCheckArgs), CampaignBot(CampaignBotArgs) }

#[derive(Clone, Debug, PartialEq)]
pub struct ServeArgs { pub config: Option<PathBuf>, pub port: Option<u16>, pub map_port: bool, pub log: Option<PathBuf> }

/// Link conditioner presets for gates (L915, L1031, L1036).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum LinkPreset {
    Clean,
    /// 150 ms, 5% loss, 30 ms jitter.
    Gate,
    /// 300 ms, 10% loss, 50 ms jitter: must never be dropped.
    Tolerated,
    /// 600 ms: must be dropped within 20 s.
    DropPing,
    /// 25% loss: must be dropped within 20 s.
    DropLoss,
    /// Host upload limited to 1 Mbps (use with 7 clients).
    Upload1Mbps,
}

#[derive(Clone, Debug, PartialEq)]
pub struct SoakArgs { pub minutes: f64, pub clients: u8, pub link: LinkPreset, pub scene: sim_systems::editor::trials::ReferenceScene, pub seed: u64, pub udp: bool, pub synthetic_tick_ms: f32, pub out: Option<PathBuf> }

#[derive(Clone, Debug, PartialEq)]
pub struct ReferenceArgs { pub scene: sim_systems::editor::trials::ReferenceScene, pub seed: u64, pub ticks: u64, pub subsystems: bool, pub threads: u8, pub cpu_clock: bool }

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum BatteryKind { OnSave, Ci, Short }

#[derive(Clone, Debug, PartialEq)]
pub struct TrialsArgs { pub blueprint: PathBuf, pub spec: BatteryKind, pub out: Option<PathBuf> }

#[derive(Clone, Debug, PartialEq)]
pub struct FitCheckArgs { pub blueprint: PathBuf, pub brief: PathBuf, pub seconds: f64, pub out: Option<PathBuf> }

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ResolverKind { Full, Abstract }

#[derive(Clone, Debug, PartialEq)]
pub struct CampaignBotArgs {
    pub policy: sim_systems::theatre::bot::Policy,
    pub seed: u64,
    pub runs: u32,
    pub until_act: content::enums::Act,
    pub players: u8,
    pub resolver: ResolverKind,
    /// (index, count): run only seeds where `seed_index % count == index`, for sharding under 6 h per job.
    pub shard: (u32, u32),
    pub max_hours: u64,
    pub out: Option<PathBuf>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ArgError {
    NoCommand,
    UnknownCommand(String),
    UnknownFlag { command: &'static str, flag: String },
    MissingValue(String),
    BadValue { flag: String, value: String },
    Required(&'static str),
}

impl std::fmt::Display for ArgError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            ArgError::NoCommand => write!(f, "no command given"),
            ArgError::UnknownCommand(c) => write!(f, "unknown command `{c}`"),
            ArgError::UnknownFlag { command, flag } => write!(f, "unknown flag `{flag}` for `{command}`"),
            ArgError::MissingValue(flag) => write!(f, "flag `{flag}` needs a value"),
            ArgError::BadValue { flag, value } => write!(f, "bad value `{value}` for `{flag}`"),
            ArgError::Required(flag) => write!(f, "missing required flag `{flag}`"),
        }
    }
}


/// Command-line spelling of a reference scene, shared by the parser, the usage text and the hash line.
pub(crate) fn scene_name(scene: sim_systems::editor::trials::ReferenceScene) -> &'static str {
    use sim_systems::editor::trials::ReferenceScene as S;
    match scene { S::ReferenceBattle => "reference", S::CoopReference => "coop", S::MinimumSwarm => "swarm" }
}

/// Command-line spelling of a link preset (also the `link` field of soak reports).
pub(crate) fn link_name(link: LinkPreset) -> &'static str {
    match link {
        LinkPreset::Clean => "clean",
        LinkPreset::Gate => "gate",
        LinkPreset::Tolerated => "tolerated",
        LinkPreset::DropPing => "drop-ping",
        LinkPreset::DropLoss => "drop-loss",
        LinkPreset::Upload1Mbps => "upload-1mbps",
    }
}

/// Command-line spelling of a kill method for `single:<method>`.
pub(crate) fn method_name(m: content::campaign::KillMethod) -> &'static str {
    use content::campaign::KillMethod as K;
    match m {
        K::Gunfire => "gunfire",
        K::Missile => "missile",
        K::Torpedo => "torpedo",
        K::Mine => "mine",
        K::DepthCharge => "depth-charge",
        K::Fire => "fire",
        K::Capture => "capture",
        K::Collision => "collision",
    }
}

/// Command-line spelling of a campaign bot policy (also the `policy` field of bot summaries).
pub(crate) fn policy_name(p: sim_systems::theatre::bot::Policy) -> String {
    use sim_systems::theatre::bot::Policy as P;
    match p {
        P::Aggressive => "aggressive".into(),
        P::Careful => "careful".into(),
        P::Mixed => "mixed".into(),
        P::AllGhost => "all-ghost".into(),
        P::NoGhost => "no-ghost".into(),
        P::SingleMethod(m) => format!("single:{}", method_name(m)),
    }
}

const SCENES: [sim_systems::editor::trials::ReferenceScene; 3] = {
    use sim_systems::editor::trials::ReferenceScene as S;
    [S::ReferenceBattle, S::CoopReference, S::MinimumSwarm]
};
const LINKS: [LinkPreset; 6] = [LinkPreset::Clean, LinkPreset::Gate, LinkPreset::Tolerated, LinkPreset::DropPing, LinkPreset::DropLoss, LinkPreset::Upload1Mbps];
const METHODS: [content::campaign::KillMethod; 8] = {
    use content::campaign::KillMethod as K;
    [K::Gunfire, K::Missile, K::Torpedo, K::Mine, K::DepthCharge, K::Fire, K::Capture, K::Collision]
};

/// A flag a command accepts: its name without the dashes, and whether it takes a value.
type FlagSpec = (&'static str, bool);

const SERVE_FLAGS: &[FlagSpec] = &[("config", true), ("port", true), ("no-map", false), ("log", true)];
const SOAK_FLAGS: &[FlagSpec] = &[("minutes", true), ("clients", true), ("link", true), ("scene", true), ("seed", true), ("udp", false), ("synthetic-tick-ms", true), ("out", true)];
const REFERENCE_FLAGS: &[FlagSpec] = &[("scene", true), ("seed", true), ("ticks", true), ("subsystems", false), ("threads", true), ("cpu-clock", false)];
const TRIALS_FLAGS: &[FlagSpec] = &[("blueprint", true), ("spec", true), ("out", true)];
const FIT_CHECK_FLAGS: &[FlagSpec] = &[("blueprint", true), ("brief", true), ("seconds", true), ("out", true)];
const CAMPAIGN_BOT_FLAGS: &[FlagSpec] = &[("policy", true), ("seed", true), ("runs", true), ("until-act", true), ("players", true), ("resolver", true), ("shard", true), ("max-hours", true), ("out", true)];

/// The flags given to one command, after syntax checks. A flag given twice keeps its last value.
struct Flags { values: std::collections::BTreeMap<&'static str, String>, switches: std::collections::BTreeSet<&'static str> }

impl Flags {
    fn parse(command: &'static str, spec: &[FlagSpec], rest: &[String]) -> Result<Flags, ArgError> {
        let mut flags = Flags { values: Default::default(), switches: Default::default() };
        let mut i = 0;
        while i < rest.len() {
            let token = rest[i].as_str();
            i += 1;
            let Some(body) = token.strip_prefix("--").filter(|b| !b.is_empty()) else {
                return Err(ArgError::UnknownFlag { command, flag: token.to_string() });
            };
            let (name, inline) = match body.split_once('=') { Some((n, v)) => (n, Some(v)), None => (body, None) };
            let Some(&(name, takes_value)) = spec.iter().find(|(n, _)| *n == name) else {
                return Err(ArgError::UnknownFlag { command, flag: format!("--{name}") });
            };
            if !takes_value {
                if let Some(v) = inline { return Err(ArgError::BadValue { flag: format!("--{name}"), value: v.to_string() }); }
                flags.switches.insert(name);
                continue;
            }
            let value = match inline {
                Some(v) => v.to_string(),
                // A following flag is never a value: no flag value of ours starts with two dashes.
                None => match rest.get(i) {
                    Some(next) if !next.starts_with("--") => { i += 1; next.clone() }
                    _ => return Err(ArgError::MissingValue(format!("--{name}"))),
                },
            };
            if value.is_empty() { return Err(ArgError::MissingValue(format!("--{name}"))); }
            flags.values.insert(name, value);
        }
        Ok(flags)
    }

    fn switch(&self, name: &str) -> bool { self.switches.contains(name) }

    /// Parse a value flag with `f`, which returns `None` for a value it rejects.
    fn get<T>(&self, name: &str, f: impl FnOnce(&str) -> Option<T>) -> Result<Option<T>, ArgError> {
        match self.values.get(name) {
            None => Ok(None),
            Some(v) => f(v).map(Some).ok_or_else(|| ArgError::BadValue { flag: format!("--{name}"), value: v.clone() }),
        }
    }

    fn or<T>(&self, name: &str, default: T, f: impl FnOnce(&str) -> Option<T>) -> Result<T, ArgError> { Ok(self.get(name, f)?.unwrap_or(default)) }

    fn path(&self, name: &str) -> Option<PathBuf> { self.values.get(name).map(PathBuf::from) }

    fn required_path(&self, name: &'static str, flag: &'static str) -> Result<PathBuf, ArgError> { self.path(name).ok_or(ArgError::Required(flag)) }
}

fn int<T: std::str::FromStr + PartialOrd>(min: T, max: T) -> impl FnOnce(&str) -> Option<T> {
    move |s| s.parse::<T>().ok().filter(|v| *v >= min && *v <= max)
}

/// A finite float strictly above `min` (`inclusive` allows `min` itself).
fn float<T: std::str::FromStr + Into<f64> + Copy>(min: f64, inclusive: bool) -> impl FnOnce(&str) -> Option<T> {
    move |s| s.parse::<T>().ok().filter(|v| { let x: f64 = (*v).into(); x.is_finite() && if inclusive { x >= min } else { x > min } })
}

fn scene(s: &str) -> Option<sim_systems::editor::trials::ReferenceScene> { SCENES.into_iter().find(|sc| scene_name(*sc) == s) }
fn link(s: &str) -> Option<LinkPreset> { LINKS.into_iter().find(|l| link_name(*l) == s) }
fn policy(s: &str) -> Option<sim_systems::theatre::bot::Policy> {
    use sim_systems::theatre::bot::Policy as P;
    Some(match s {
        "aggressive" => P::Aggressive,
        "careful" => P::Careful,
        "mixed" => P::Mixed,
        "all-ghost" => P::AllGhost,
        "no-ghost" => P::NoGhost,
        _ => P::SingleMethod(METHODS.into_iter().find(|m| s.strip_prefix("single:") == Some(method_name(*m)))?),
    })
}
fn act(s: &str) -> Option<content::enums::Act> { content::enums::Act::ALL.into_iter().find(|a| s.parse::<u8>().ok() == Some(a.number())) }
fn shard(s: &str) -> Option<(u32, u32)> {
    let (i, n) = s.split_once('/')?;
    let (i, n) = (i.parse::<u32>().ok()?, n.parse::<u32>().ok()?);
    (n >= 1 && i < n).then_some((i, n))
}

/// Parse the arguments after the program name. Unit: `server-args`.
///
/// `help`, `--help` or `-h` anywhere wins over everything else, including errors elsewhere on the line;
/// then `version` or `--version` anywhere. A flag given twice keeps its last value. Numbers are range
/// checked: ports 1 to 65535, thread, client, player and run counts at least 1, durations finite and above
/// zero, the synthetic tick cost finite and not negative, and a shard `I/N` with `I < N`.
pub fn parse(args: &[String]) -> Result<Command, ArgError> {
    if args.iter().any(|a| matches!(a.as_str(), "help" | "--help" | "-h")) { return Ok(Command::Help); }
    if args.iter().any(|a| matches!(a.as_str(), "version" | "--version")) { return Ok(Command::Version); }
    let Some((command, rest)) = args.split_first() else { return Err(ArgError::NoCommand) };
    use sim_systems::editor::trials::ReferenceScene;
    match command.as_str() {
        "serve" => {
            let f = Flags::parse("serve", SERVE_FLAGS, rest)?;
            Ok(Command::Serve(ServeArgs { config: f.path("config"), port: f.get("port", int(1u16, u16::MAX))?, map_port: !f.switch("no-map"), log: f.path("log") }))
        }
        "soak" => {
            let f = Flags::parse("soak", SOAK_FLAGS, rest)?;
            Ok(Command::Soak(SoakArgs {
                minutes: f.or("minutes", 10.0, float::<f64>(0.0, false))?,
                clients: f.or("clients", 2, int(1u8, u8::MAX))?,
                link: f.or("link", LinkPreset::Clean, link)?,
                scene: f.or("scene", ReferenceScene::ReferenceBattle, scene)?,
                seed: f.or("seed", 1, int(0u64, u64::MAX))?,
                udp: f.switch("udp"),
                synthetic_tick_ms: f.or("synthetic-tick-ms", 0.0, float::<f32>(0.0, true))?,
                out: f.path("out"),
            }))
        }
        "reference-battle" => {
            let f = Flags::parse("reference-battle", REFERENCE_FLAGS, rest)?;
            let threads = f.or("threads", 3, int(1u8, u8::MAX))?;
            let cpu_clock = f.switch("cpu-clock");
            // A thread's CPU clock sees only the work on that thread: with more than one sim worker it would miss
            // the parallel phases.
            if cpu_clock && threads != 1 { return Err(ArgError::BadValue { flag: "--threads".into(), value: format!("{threads} (--cpu-clock needs 1)") }); }
            Ok(Command::Reference(ReferenceArgs {
                scene: f.or("scene", ReferenceScene::ReferenceBattle, scene)?,
                seed: f.or("seed", 1, int(0u64, u64::MAX))?,
                ticks: f.or("ticks", 3600, int(0u64, u64::MAX))?,
                subsystems: f.switch("subsystems"),
                threads,
                cpu_clock,
            }))
        }
        "trials" => {
            let f = Flags::parse("trials", TRIALS_FLAGS, rest)?;
            let spec = f.or("spec", BatteryKind::Ci, |s| match s { "on-save" => Some(BatteryKind::OnSave), "ci" => Some(BatteryKind::Ci), "short" => Some(BatteryKind::Short), _ => None })?;
            Ok(Command::Trials(TrialsArgs { blueprint: f.required_path("blueprint", "--blueprint")?, spec, out: f.path("out") }))
        }
        "fit-check" => {
            let f = Flags::parse("fit-check", FIT_CHECK_FLAGS, rest)?;
            Ok(Command::FitCheck(FitCheckArgs {
                blueprint: f.required_path("blueprint", "--blueprint")?,
                brief: f.required_path("brief", "--brief")?,
                seconds: f.or("seconds", 30.0, float::<f64>(0.0, false))?,
                out: f.path("out"),
            }))
        }
        "campaign-bot" => {
            let f = Flags::parse("campaign-bot", CAMPAIGN_BOT_FLAGS, rest)?;
            Ok(Command::CampaignBot(CampaignBotArgs {
                policy: f.or("policy", sim_systems::theatre::bot::Policy::Mixed, policy)?,
                seed: f.or("seed", 1, int(0u64, u64::MAX))?,
                runs: f.or("runs", 1, int(1u32, u32::MAX))?,
                until_act: f.or("until-act", content::enums::Act::Act1, act)?,
                players: f.or("players", 1, int(1u8, u8::MAX))?,
                resolver: f.or("resolver", ResolverKind::Full, |s| match s { "full" => Some(ResolverKind::Full), "abstract" => Some(ResolverKind::Abstract), _ => None })?,
                shard: f.or("shard", (0, 1), shard)?,
                max_hours: f.or("max-hours", 10_000, int(1u64, u64::MAX))?,
                out: f.path("out"),
            }))
        }
        other => Err(ArgError::UnknownCommand(other.to_string())),
    }
}

/// The usage text, matching the table above. Unit: `server-args`.
pub fn usage() -> &'static str {
    "usage: black-pennant-server <command> [flags]

Flags are `--name value` or `--name=value`; switches are bare `--name`.

commands:
  serve               run a dedicated server
      --config PATH       dedicated server RON config (defaults when omitted)
      --port N            UDP port, overrides the config (47610)
      --no-map            skip port mapping
      --log PATH          connection log file (connection.log beside the config)
  soak                server and bot clients in one process
      --minutes F         simulated minutes (10)
      --clients N         bot clients (2)
      --link PRESET       clean, gate, tolerated, drop-ping, drop-loss or upload-1mbps (clean)
      --scene SCENE       reference, coop or swarm (reference)
      --seed N            session seed (1)
      --udp               loopback UDP instead of the in-memory transport
      --synthetic-tick-ms F  injected tick cost in milliseconds (0)
      --out PATH          RON report (stdout when omitted)
  reference-battle    run a reference scene headless and print its state hash
      --scene SCENE       reference, coop or swarm (reference)
      --seed N            scene seed (1)
      --ticks N           ticks to run (3600)
      --subsystems        also print one hash per subsystem
      --threads N         sim pool threads (3)
      --cpu-clock         time by the stepping thread's CPU clock (needs --threads 1; for loaded machines)
  trials              run the trial battery on a blueprint
      --blueprint PATH    blueprint file (required)
      --spec SPEC         on-save, ci or short (ci)
      --out PATH          RON scorecard (stdout when omitted)
  fit-check           run Fit Check on a blueprint against a briefing
      --blueprint PATH    blueprint file (required)
      --brief PATH        RON fit brief (required)
      --seconds F         simulated seconds (30)
      --out PATH          RON result (stdout when omitted)
  campaign-bot        play the campaign headless with a bot policy
      --policy P          aggressive, careful, mixed, all-ghost, no-ghost or single:METHOD (mixed);
                          METHOD is gunfire, missile, torpedo, mine, depth-charge, fire, capture or collision
      --seed N            first seed (1)
      --runs N            runs, seeds seed to seed+runs-1 (1)
      --until-act N       stop after act 1 to 4 (1)
      --players N         players (1)
      --resolver R        full or abstract (full)
      --shard I/N         run only runs where index % N == I (0/1)
      --max-hours N       theatre hour limit per run (10000)
      --out PATH          RON summary (stdout when omitted)
  help                print this text
  version             print the version and protocol

exit codes: 0 pass, 1 a check or gate failed, 2 usage error, 3 runtime error"
}

#[cfg(test)]
mod tests {
    use super::*;
    use sim_systems::editor::trials::ReferenceScene;
    use sim_systems::theatre::bot::Policy;

    fn p(line: &str) -> Result<Command, ArgError> { parse(&line.split_whitespace().map(String::from).collect::<Vec<_>>()) }

    #[test]
    fn ci_reference_battle_line() {
        assert_eq!(p("reference-battle --ticks 3600"), Ok(Command::Reference(ReferenceArgs { scene: ReferenceScene::ReferenceBattle, seed: 1, ticks: 3600, subsystems: false, threads: 3, cpu_clock: false })));
        assert_eq!(p("reference-battle --scene=swarm --seed 7 --ticks=0 --subsystems --threads 1"), Ok(Command::Reference(ReferenceArgs { scene: ReferenceScene::MinimumSwarm, seed: 7, ticks: 0, subsystems: true, threads: 1, cpu_clock: false })));
    }

    #[test]
    fn help_and_version_win() {
        assert_eq!(p("soak --bogus help"), Ok(Command::Help));
        assert_eq!(p("-h"), Ok(Command::Help));
        assert_eq!(p("reference-battle --help"), Ok(Command::Help));
        assert_eq!(p("version"), Ok(Command::Version));
        assert_eq!(p("serve --version"), Ok(Command::Version));
        assert_eq!(p(""), Err(ArgError::NoCommand));
        assert_eq!(p("launch"), Err(ArgError::UnknownCommand("launch".into())));
    }

    #[test]
    fn syntax_errors() {
        assert_eq!(p("reference-battle --ticks"), Err(ArgError::MissingValue("--ticks".into())));
        assert_eq!(p("reference-battle --ticks --subsystems"), Err(ArgError::MissingValue("--ticks".into())));
        assert_eq!(p("reference-battle --ticks="), Err(ArgError::MissingValue("--ticks".into())));
        assert_eq!(p("reference-battle --subsystems=yes"), Err(ArgError::BadValue { flag: "--subsystems".into(), value: "yes".into() }));
        assert_eq!(p("reference-battle 3600"), Err(ArgError::UnknownFlag { command: "reference-battle", flag: "3600".into() }));
        assert_eq!(p("reference-battle --port 1"), Err(ArgError::UnknownFlag { command: "reference-battle", flag: "--port".into() }));
        assert_eq!(p("reference-battle --threads 0"), Err(ArgError::BadValue { flag: "--threads".into(), value: "0".into() }));
        assert!(matches!(p("reference-battle --threads 1 --cpu-clock"), Ok(Command::Reference(ReferenceArgs { cpu_clock: true, threads: 1, .. }))));
        assert!(matches!(p("reference-battle --cpu-clock"), Err(ArgError::BadValue { .. })), "the CPU clock needs one thread");
        assert!(matches!(p("reference-battle --ticks -5"), Err(ArgError::BadValue { .. })));
        assert!(matches!(p("reference-battle --ticks=-5"), Err(ArgError::BadValue { .. })));
        assert!(matches!(p("serve --port 0"), Err(ArgError::BadValue { .. })));
        assert!(matches!(p("serve --port 70000"), Err(ArgError::BadValue { .. })));
        for bad in ["nan", "inf", "0", "-1"] { assert!(matches!(p(&format!("soak --minutes={bad}")), Err(ArgError::BadValue { .. })), "{bad}"); }
        assert!(matches!(p("soak --synthetic-tick-ms=-0.5"), Err(ArgError::BadValue { .. })));
        assert!(matches!(p("soak --link wifi"), Err(ArgError::BadValue { .. })));
        assert_eq!(p("trials --spec ci"), Err(ArgError::Required("--blueprint")));
        assert_eq!(p("fit-check --blueprint a.bp.ron"), Err(ArgError::Required("--brief")));
    }

    #[test]
    fn last_value_wins() {
        let Ok(Command::Reference(a)) = p("reference-battle --seed 2 --seed 3") else { panic!() };
        assert_eq!(a.seed, 3);
    }

    #[test]
    fn soak_and_serve_defaults_and_values() {
        assert_eq!(p("soak"), Ok(Command::Soak(SoakArgs { minutes: 10.0, clients: 2, link: LinkPreset::Clean, scene: ReferenceScene::ReferenceBattle, seed: 1, udp: false, synthetic_tick_ms: 0.0, out: None })));
        for l in LINKS { let Ok(Command::Soak(a)) = p(&format!("soak --link {} --udp --clients 7 --minutes 0.5 --synthetic-tick-ms 9 --out r.ron", link_name(l))) else { panic!() }; assert_eq!((a.link, a.udp, a.clients, a.minutes, a.synthetic_tick_ms, a.out), (l, true, 7, 0.5, 9.0, Some(PathBuf::from("r.ron")))); }
        assert_eq!(p("serve"), Ok(Command::Serve(ServeArgs { config: None, port: None, map_port: true, log: None })));
        assert_eq!(p("serve --config s.ron --port 47611 --no-map --log c.log"), Ok(Command::Serve(ServeArgs { config: Some("s.ron".into()), port: Some(47611), map_port: false, log: Some("c.log".into()) })));
    }

    #[test]
    fn trials_and_fit_check() {
        assert_eq!(p("trials --blueprint a.bp.ron"), Ok(Command::Trials(TrialsArgs { blueprint: "a.bp.ron".into(), spec: BatteryKind::Ci, out: None })));
        assert_eq!(p("trials --blueprint a --spec on-save --out o"), Ok(Command::Trials(TrialsArgs { blueprint: "a".into(), spec: BatteryKind::OnSave, out: Some("o".into()) })));
        assert_eq!(p("fit-check --blueprint a --brief b"), Ok(Command::FitCheck(FitCheckArgs { blueprint: "a".into(), brief: "b".into(), seconds: 30.0, out: None })));
    }

    #[test]
    fn campaign_bot_policies_acts_and_shards() {
        let Ok(Command::CampaignBot(a)) = p("campaign-bot") else { panic!() };
        assert_eq!((a.policy, a.seed, a.runs, a.until_act, a.players, a.resolver, a.shard, a.max_hours, a.out), (Policy::Mixed, 1, 1, content::enums::Act::Act1, 1, ResolverKind::Full, (0, 1), 10_000, None));
        for m in METHODS {
            let Ok(Command::CampaignBot(a)) = p(&format!("campaign-bot --policy single:{}", method_name(m))) else { panic!("{m:?}") };
            assert_eq!(a.policy, Policy::SingleMethod(m));
            assert_eq!(policy_name(a.policy), format!("single:{}", method_name(m)));
        }
        for (s, pol) in [("aggressive", Policy::Aggressive), ("careful", Policy::Careful), ("all-ghost", Policy::AllGhost), ("no-ghost", Policy::NoGhost)] {
            let Ok(Command::CampaignBot(a)) = p(&format!("campaign-bot --policy={s}")) else { panic!("{s}") };
            assert_eq!(a.policy, pol);
            assert_eq!(policy_name(pol), s);
        }
        assert!(matches!(p("campaign-bot --policy single:"), Err(ArgError::BadValue { .. })));
        assert!(matches!(p("campaign-bot --policy single:laser"), Err(ArgError::BadValue { .. })));
        let Ok(Command::CampaignBot(a)) = p("campaign-bot --until-act 4 --shard 2/3 --resolver abstract --players 4 --runs 200") else { panic!() };
        assert_eq!((a.until_act, a.shard, a.resolver, a.players, a.runs), (content::enums::Act::Act4, (2, 3), ResolverKind::Abstract, 4, 200));
        for bad in ["--until-act 0", "--until-act 5", "--shard 3/3", "--shard 0/0", "--shard 1", "--runs 0", "--players 0"] {
            assert!(matches!(p(&format!("campaign-bot {bad}")), Err(ArgError::BadValue { .. })), "{bad}");
        }
    }

    #[test]
    fn scene_names_round_trip() {
        for s in SCENES { assert_eq!(scene(scene_name(s)), Some(s)); assert!(usage().contains(scene_name(s))); }
        for l in LINKS { assert!(usage().contains(link_name(l))); }
        for m in METHODS { assert!(usage().contains(method_name(m))); }
    }
}
