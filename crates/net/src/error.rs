//! Contract-owned (no work unit). Errors and typed reasons. Reasons that reach a player are enums the client
//! turns into translation keys (`net.refusal.*`, `net.drop.*`); net never produces English text.
//! DESIGN.md L1001-1003 (join, version mismatch, kick, ban), L1031 (saturation refuses joins), L1035 (drop rule).
use serde::{Deserialize, Serialize};

/// Why the host refused a join. Sent in [`crate::msg::ServerMsg::Refused`] before the host disconnects.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum JoinRefusal {
    /// Protocol versions differ; the message names both (L1003).
    ProtocolMismatch { host: u32, client: u32 },
    /// The lobby is at its player limit (mode maximum or the upload-derived client limit, L1031).
    LobbyFull { max_players: u8 },
    /// The host's upload is saturated; new joins are refused while the HUD shows it (L1031).
    HostSaturated,
    /// This client id is banned for the session (L1003).
    Banned,
    /// Campaign co-op: joining happens between operations only (L834). Rejoin into a held slot is allowed.
    OperationInProgress,
    /// PvP: the match has started; sandbox accepts joins at any time (L961).
    MatchInProgress,
    /// The same install is already connected.
    DuplicateClient,
    /// The hello was malformed or arrived on the wrong channel.
    BadHello,
    /// The host is shutting the session down.
    ShuttingDown,
}

/// Why a host removed a connected client.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum DropReason {
    /// The host kicked the player (L1003).
    Kicked,
    /// The host kicked and banned the client id for the session.
    Banned,
    /// RTT above 500 ms for the hold time (L1035).
    HighPing,
    /// Loss above 20% for the hold time.
    PacketLoss,
    /// More than 30 snapshots (1 s) behind for the hold time.
    FallingBehind,
    /// No packets: renet2 timed the connection out.
    TimedOut,
    /// The player left.
    Left,
    /// The host regenerated the lobby secret; old codes no longer connect (L1001). Connected players stay.
    SecretRegenerated,
}

/// Why a session ended for a client.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum SessionEnd {
    HostQuit,
    HostLost,
    Dropped(DropReason),
    Refused,
    MatchOver,
}

#[derive(Debug, thiserror::Error)]
pub enum NetError {
    #[error("io: {0}")]
    Io(#[from] std::io::Error),
    #[error("transport: {0}")]
    Transport(String),
    #[error("decode: {0}")]
    Decode(String),
    #[error("encode: {0}")]
    Encode(String),
    #[error("join code: {0}")]
    JoinCode(JoinCodeError),
    #[error("join refused: {0:?}")]
    Refused(JoinRefusal),
    #[error("blueprint: {0}")]
    Blueprint(String),
    #[error("simulation: {0}")]
    Sim(#[from] sim_systems::SimError),
    #[error("replay: {0}")]
    Replay(#[from] sim_systems::replay::ReplayError),
    #[error("not connected")]
    NotConnected,
    #[error("wrong session phase for this operation")]
    WrongPhase,
}

/// Join code and lobby key parse failures (L1001). The client shows each as its own key.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, thiserror::Error)]
pub enum JoinCodeError {
    #[error("bad character")]
    BadCharacter,
    #[error("wrong length")]
    WrongLength,
    #[error("checksum mismatch")]
    Checksum,
    #[error("unknown join code format {0}")]
    UnknownFormat(u8),
    /// The code names a different protocol version: refuse before connecting, naming both.
    #[error("join code is for protocol {code}, this build speaks {ours}")]
    ProtocolMismatch { code: u32, ours: u32 },
    #[error("bad address")]
    BadAddress,
}

pub type NetResult<T> = Result<T, NetError>;
