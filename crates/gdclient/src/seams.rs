//! Contract-owned (no work unit). Typed seams to things this crate's units share, and to the sibling crates
//! `net`, `hull-mesh` and `audio`. Every type that crosses one of those seams is the sibling crate's own type,
//! re-exported here so gdclient units import it from one place; gdclient defines a type of its own only where
//! it needs a main-thread shape the sibling crate does not have ([`ClientMsg`], [`LinkInbox`], [`LinkStatus`],
//! [`ShellPose`], the two traits).
//! DESIGN.md L27 and L899 (single-player runs the server in-process over the in-memory transport), L910-916
//! (interpolation), L982-990 (transport backends), L1088 (snapshots and a command queue between the simulation
//! and the main thread), L1927-1931 (surface bytes and the chunk volume), L1943 (hull LODs), L1242-1247
//! (rendered sound files), L1076 (generated files).
//!
//! # Who owns what at the session seam
//! | Thing | Owner | gdclient reaches it through |
//! | --- | --- | --- |
//! | The `Sim` of the running sortie, its stepping, snapshots, event routing, replay recording | `net::ServerSession` (or the in-process stand-in of `link::channel`) | [`HostPort`], on the clock thread |
//! | The lobby (`net::lobby::Lobby` with its `LobbySetup`), admission, kicks, bans | `net::ServerSession` | [`HostPort`] and [`LobbyRequest`]s from the host's own client |
//! | Snapshot decoding, interpolation in time, client-flown shells, input repetition, blueprint fetches | `net::ClientSession` (or `link::channel::Presenter`) | [`ClientLink`], on the main thread |
//! | The campaign `Theatre`, the save, 60 s checkpoints, the tick deadline, the 3-worker pool | gdclient (`BpCampaign`, `session::host`) | [`HostEvent::Theatre`] in, [`ServerMsg`] out |
//!
//! Nothing here adds to or changes `net`'s public API: `link::net` adapts `net::ClientSession` and
//! `net::ServerSession` method for method (each trait method below names the call it becomes).
//!
//! # Events
//! Client-side code handles [`WireEvent`] (`tick` plus `sim_systems::protocol::SimEvent`): the audience and
//! lane of `sim_systems::protocol::OutEvent` never leave the server. Sources that produce `OutEvent`s locally
//! (the replay player, test scripts, the in-process pair) convert with `net::events::to_wire` after applying
//! `Audience::admits`.
use content::ids::{ContentHash, PlayerSlot};
use content::share::PackedBlueprint;
use glam::DVec3;
use sim_core::projectiles::ProjectileId;
use sim_systems::modes::LobbySetup;
use sim_systems::protocol::{Command, PlayerInput};
use sim_systems::theatre::TheatreCommand;
use sim_systems::Sim;

// ---------------------------------------------------------------------------------------------------------
// Types of the `net` crate used across gdclient (the one definition of each; never mirrored locally)
// ---------------------------------------------------------------------------------------------------------

pub use net::error::{DropReason, JoinCodeError, JoinRefusal, NetError, SessionEnd};
pub use net::interp::{InterpStats, InterpTuning, RenderFrame};
pub use net::msg::{
    BlueprintRefusal, CheckpointBundle, ConnectionPanel, DropWarning, LinkQuality, LobbyMember, LobbyRequest, LobbyView,
    MappingReport, PlayerLink, ServerMsg, SortieIntro, SuccessionList, TransferKind, Welcome, WireEvent,
};
pub use net::session::{ClientEvent, HostEvent, SessionPhase};
pub use net::transport::netcode::UdpHostOptions;
pub use net::transport::LinkStats;

// ---------------------------------------------------------------------------------------------------------
// Client side of a session (main thread)
// ---------------------------------------------------------------------------------------------------------

/// Main thread to the session's server, in send order. One variant per `net::ClientSession` send method;
/// the link stamps sequence numbers and the client render tick itself (the server stamps player slot and
/// RTT, L951). There is no "set name" message: the display name travels once in `net::msg::Hello`. Kick,
/// ban, upload figure and secret changes are host-only and go to the clock thread as
/// `session::host::HostControl`, never through the link.
#[derive(Clone, Debug, PartialEq)]
pub enum ClientMsg {
    /// `ClientSession::send_command(command, floor(render_tick of the last sampled frame))`.
    Command(Command),
    /// `ClientSession::set_input`. `BpSession` calls this at 30 Hz; the session sends the last 4 per packet
    /// (L1424).
    Input(PlayerInput),
    /// `ClientSession::lobby`. `LobbyRequest::SetSetup` and `Start` are honoured only from the host's own
    /// client (slot 0 over the in-memory transport).
    Lobby(LobbyRequest),
    /// `ClientSession::theatre` (co-op campaign, L832-845).
    Theatre(TheatreCommand),
    /// `ClientSession::upload_blueprint`: our design for the lobby or a refit (L943-945). Send it before
    /// `LobbyRequest::SetBlueprint` of the same hash.
    UploadBlueprint(PackedBlueprint),
    /// `ClientSession::request_blueprint`. The session already requests hashes named by reveals and by the
    /// sortie intro; this is for the lobby (another member's design preview).
    RequestBlueprint(ContentHash),
    /// `ClientSession::report_mapping`: a co-op member's own port-mapping result (L1005).
    Mapping(MappingReport),
    /// `ClientSession::request_replay`: the released PvP replay after the match (L1049).
    RequestReplay,
}

/// One client-flown shell at the render tick, from `net::interp::ShellTracker::render_positions`: world
/// metres and metres per second. What it looks like (calibre, band, tracer) comes from its
/// `PhysEvent::ProjectileSpawned` event, which the projectile renderer keeps by id.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct ShellPose { pub id: ProjectileId, pub pos: DVec3, pub vel: DVec3 }

/// Where the local end of a link stands. Mirrors `net::session::client::ClientState` without its payloads
/// (the `Welcome` arrives once as [`ClientEvent::Welcomed`]).
#[derive(Clone, Debug, Default, PartialEq)]
pub enum LinkState {
    /// No session, or closed by `close()`.
    #[default]
    Closed,
    Connecting,
    Joined,
    Refused(JoinRefusal),
    Ended(SessionEnd),
}

/// State of the local end of a link. Per-player numbers for the connection panel are not here: they arrive
/// at 1 Hz as [`ClientEvent::Connection`] ([`ConnectionPanel`], L1033) on every link, the host's own included.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct LinkStatus {
    pub state: LinkState,
    /// `ClientSession::link()`: RTT, loss and throughput of this end. All zero on in-process and replay links.
    pub link: LinkStats,
    /// `ClientSession::buffer().stats()`: frames sampled and frames that had to extrapolate (gate: at most
    /// 1%, L915), current delay and playback rate.
    pub interp: InterpStats,
}
impl LinkStatus {
    /// Share of sampled frames that extrapolated, 0..=1.
    pub fn extrapolated(&self) -> f32 { if self.interp.frames == 0 { 0.0 } else { self.interp.extrapolated as f32 / self.interp.frames as f32 } }
}

/// Everything one `poll` produced. `BpSession` owns one and clears it before each poll.
#[derive(Clone, Debug, Default)]
pub struct LinkInbox {
    /// This frame's state, already interpolated to the playback tick (`ClientSession::sample`); `None`
    /// before the first snapshot of a sortie and outside sorties. It is a whole `Snapshot`: poses are
    /// interpolated, and the slow sections (`ship`, `tracks`, `alerts`, `objectives`, `pacing`) are whatever
    /// `net::interp::SnapshotBuffer::sample` puts there, never touched again by gdclient, so the HUD reads
    /// the same `Snapshot` the renderer does. `crate::interp::ViewBuilder` turns it into render space.
    pub frame: Option<RenderFrame>,
    /// Newest sim tick received (`ClientSession::buffer().newest_tick()`), ahead of the playback tick by the
    /// interpolation delay. `FrameFeed::snapshot_rev` increments when it changes.
    pub newest_tick: Option<u64>,
    /// Client-flown shells at `frame.render_tick`, ascending id (`ClientSession::shells()`).
    pub shells: Vec<ShellPose>,
    /// Simulation events in arrival order ([`ClientEvent::Sim`] unwrapped). The link has already applied
    /// each to its snapshot buffer and shell tracker.
    pub events: Vec<WireEvent>,
    /// Every other [`ClientEvent`], in order: welcome and refusal, lobby views, sortie loading, running and
    /// closed, the connection panel, saturation, theatre traffic, succession lists, checkpoints, blueprint
    /// and bulk arrivals, host silence, the session's end.
    pub session: Vec<ClientEvent>,
}

/// The client end of a session, whatever is behind it. The main thread owns one at most
/// (`session::Connection`). Implementations:
/// - `link::net::NetLink` (unit `gd-link-net`): a `net::ClientSession` over the in-memory transport
///   (single-player, the host's own client) or UDP (a joined client).
/// - `link::channel` (unit `gd-link-channel`): the in-process stand-in that skips the wire.
/// - `link::replay::ReplayLink` (unit `gd-link-replay`): a `sim_systems::replay::play::ReplayPlayer`.
///
/// Not `Send`: a link lives and dies on the main thread. `net::ClientSession` is non-blocking, so polling it
/// there costs one decode per arrived snapshot and one sample per frame, inside the 0.6 ms "snapshot and
/// interpolation" share of the 2 ms adapter budget (L2004).
pub trait ClientLink {
    /// Non-blocking, once per frame from `BpSession::process`. net: `ClientSession::update()` (events split
    /// into `inbox.events` and `inbox.session`), then `sample()` into `inbox.frame`,
    /// `shells().render_positions(frame.render_tick, ..)` into `inbox.shells`, `buffer().newest_tick()`.
    /// `now_s` is `FrameFeed::now_s`; a net link reads the same instant from the `net::clock::Clock` it was
    /// built with and ignores the argument, the other links use it as their clock. A transport error ends
    /// the link: `status().state` becomes `Ended(SessionEnd::HostLost)` and [`ClientEvent::Ended`] is
    /// appended once.
    fn poll(&mut self, now_s: f64, inbox: &mut LinkInbox);
    /// Queue a message; never blocks. Dropped unless `status().state` is `Joined`.
    fn send(&mut self, msg: ClientMsg);
    fn status(&self) -> LinkStatus;
    /// A blueprint the local store holds (after [`ClientEvent::BlueprintReady`], or one of our own):
    /// `ClientSession::store_mut().get(hash)` wrapped with its hash. `BpSession` hands it to the background
    /// worker as `worker::Job::BuildReceived`.
    fn blueprint(&mut self, hash: &ContentHash) -> Option<PackedBlueprint>;
    /// Leave politely and release the socket (`ClientSession::leave`). Idempotent.
    fn close(&mut self);
}

// ---------------------------------------------------------------------------------------------------------
// Host side of a session (clock thread)
// ---------------------------------------------------------------------------------------------------------

/// The server end the host driver (`session::host`, unit `gd-host`) pumps on the clock thread. The port owns
/// the `Sim` while a sortie runs and does everything `net::session::server` documents for `update`; the
/// driver owns the tick deadline, builds each `Sim`, and relays [`HostEvent`]s to the main thread.
/// Implementations: `link::net::NetHost` (a `net::ServerSession`; every method below is the call named in
/// its comment) and `link::channel` (one local player, no wire, no lobby). `Send` because it is built on the
/// main thread and moved to the clock thread.
pub trait HostPort: Send {
    /// One pass of the host loop: `ServerSession::update`. In [`SessionPhase::Sortie`] it applies inputs and
    /// commands, steps the `Sim` the ticks its clock owes (at most `ServerOptions::max_catch_up_ticks`,
    /// which gdclient sets to 4, L1363) inside the sim pool it was built with, routes events by audience,
    /// sends each player its snapshot at 30 Hz and records the replay. Appends what the app must act on.
    fn update(&mut self, events: &mut Vec<HostEvent>) -> Result<(), NetError>;
    /// `ServerSession::phase`.
    fn phase(&self) -> SessionPhase;
    /// `ServerSession::set_phase`: between `Lobby`, `Theatre` and `AfterAction`. `Loading` and `Sortie` are
    /// entered by [`HostPort::start_sortie`].
    fn set_phase(&mut self, phase: SessionPhase) -> Result<(), NetError>;
    /// `ServerSession::start_sortie`. The driver first puts every blueprint the sortie's config names into
    /// the store with [`HostPort::put_blueprint`], so clients can fetch their own side's.
    fn start_sortie(&mut self, sim: Sim) -> Result<(), NetError>;
    /// `ServerSession::sim`: the running sortie, for checkpoints (`Sim::checkpoint`), pacing and results.
    fn sim(&self) -> Option<&Sim>;
    /// `ServerSession::sim_mut`: for `Sim::finish` when the main thread stops the sortie.
    fn sim_mut(&mut self) -> Option<&mut Sim>;
    /// `ServerSession::end_sortie`: the `Sim` back (read `Sim::result`) and the replay bytes if one was
    /// recorded. The in-process stand-in records none.
    fn end_sortie(&mut self) -> Option<(Sim, Option<Vec<u8>>)>;
    /// `ServerSession::send_to` (`Some`) or `broadcast` (`None`): theatre events, chart, briefing,
    /// after-action and succession lists on the `Session` channel.
    fn send(&mut self, to: Option<PlayerSlot>, msg: ServerMsg);
    /// `ServerSession::send_bulk`: checkpoints (`TransferKind::Checkpoint`, bytes from
    /// `net::coop::CoopHost::bundle`), save copies, released replays.
    fn send_bulk(&mut self, to: Option<PlayerSlot>, kind: TransferKind, bytes: Vec<u8>);
    /// `ServerSession::lobby().setup().clone()`: what a lobby-started sortie is built from
    /// (`session::host::HostStart::Lobby`).
    fn lobby_setup(&self) -> LobbySetup;
    /// `ServerSession::store_mut().get(hash)`: compressed bytes of a member's validated design.
    fn blueprint(&mut self, hash: &ContentHash) -> Option<Vec<u8>>;
    /// `ServerSession::store_mut().put(hash, compressed)`.
    fn put_blueprint(&mut self, packed: PackedBlueprint);
    /// `ServerSession::kick` (L1003).
    fn kick(&mut self, slot: PlayerSlot, ban: bool);
    /// `ServerSession::set_upload_kbps` (L1031).
    fn set_upload_kbps(&mut self, kbps: u32);
    /// Start, replace or stop the UDP backend. `Some`: drop the current UDP transport if any
    /// (`ServerSession::remove_transport(index, reason)`), then
    /// `net::transport::netcode::NetcodeServerBackend::bind_udp(&options, clock.unix_now(), None)` and
    /// `add_transport`; returns the addresses it listens on. `None`: only remove. `reason` is
    /// `DropReason::SecretRegenerated` when replacing for a new lobby secret (L1001), `DropReason::Left`
    /// when hosting stops. The in-process stand-in returns `NetError::WrongPhase`.
    fn set_udp(&mut self, options: Option<UdpHostOptions>, reason: DropReason) -> Result<Vec<std::net::SocketAddr>, NetError>;
    /// `ServerSession::connection_panel`: also the source of the driver's tick statistics
    /// (`host_tick_hz`, `host_tick_p95_ms`).
    fn connection_panel(&self) -> ConnectionPanel;
    /// `ServerSession::log_mut().take_unflushed_ron()`: new connection-log lines as RON (L1039); empty when
    /// there are none.
    fn drain_log(&mut self) -> String;
    /// `ServerSession::shutdown`: tell everyone the host quit, disconnect, flush. Idempotent.
    fn shutdown(&mut self);
}

// ---------------------------------------------------------------------------------------------------------
// hull-mesh seam
// ---------------------------------------------------------------------------------------------------------

/// Output types of the `hull-mesh` crate that cross from the meshing thread or the loader to the main thread.
/// gdclient defines none of its own.
///
/// | What is drawn | hull-mesh call | Type | Vertex format |
/// | --- | --- | --- | --- |
/// | Hull at LOD0, one mesh per 16 m chunk | `pipeline::ChunkMesher::mesh_chunk` | [`ChunkMesh`] (`surface`, `shadow`, `volume`) | `format::CHUNK_SURFACE`: position, octahedral normal, index. No colour, UV or tangent: albedo, AO, scorch and armour come from the chunk's 34 cubed RGBA8 [`VolumeTexture`] (L1927-1931) |
/// | Hull at LOD1 to LOD4, one mesh per vessel (L1943) | `lod::mesh_lod` then `surface::encode_colored_surface` | [`SurfaceBytes`] | `format::COLORED_SURFACE`: adds RGBA8 vertex colour, RGB = palette swatch times AO (linear), A = 1 |
/// | Shadow mesh of a LOD0 chunk | part of [`ChunkMesh`] | [`SurfaceBytes`] | `format::SHADOW_SURFACE`: welded positions and index only |
/// | Part and small-body models | `parts::build_part_model`, `parts::build_body_model` | `parts::PartModel` of [`MeshData`] pieces | arrays: position, normal, colour (RGB linear albedo, A paint mask), `custom0` (x emission strength, y glass flag) |
/// | VFX layer A meshes, ocean clipmap, clouds, islands | `vfx::meshes::base_mesh`, `env::ocean`, `env::scenery` | [`MeshData`] | arrays; `custom0` meaning is on each generator |
///
/// [`SurfaceBytes`] goes to `RenderingServer::mesh_create_from_surfaces` through `convert::surface_dict`;
/// [`MeshData`] goes to `mesh_add_surface_from_arrays` through `convert::mesh_arrays`.
pub use hull_mesh::pipeline::ChunkMesh;
pub use hull_mesh::{Aabb, ChunkKey, Lod, MaterialClass, MeshData, MeshError, QualityTier, SurfaceBytes, VolumeTexture};

// ---------------------------------------------------------------------------------------------------------
// audio seam
// ---------------------------------------------------------------------------------------------------------

/// Sound ids are `audio::SoundId`: the `audio` crate renders one 48 kHz 16-bit WAV per id through
/// `cargo xtask gen` and its `SoundId::spec()` table is the single list of stems, buses, loop flags and
/// positional flags (L1242-1247). gdclient keeps no list of its own. Nothing is synthesized live; Rust only
/// picks the file, the pitch and the volume, with the rules in `audio::mix`.
///
/// What GDScript sees is the stem as a `StringName` (`AudioEventDict.sound`), for example
/// `&"weapon_gun_medium"`, `&"ambience_engine_diesel"`, `&"radio_squelch_open"`, `&"alert_warning"`,
/// `&"ui_click"`; the file is `res://audio/gen/<stem>.wav`. Interface sounds are requested by
/// `Bp.UI_SOUND_*` (`audio::UiSfx`), never by a typed string.
pub mod sound {
    pub use audio::mix;
    pub use audio::{AlertSound, AmbienceSfx, Bus, ImpactSfx, Playback, RadioSfx, SoundId, SoundSpec, UiSfx, WeaponSfx};

    /// File stem of a sound: `AudioEventDict.sound` and `AudioLoopDict.sound`.
    pub fn stem(id: SoundId) -> &'static str { id.spec().stem }
    /// Every stem this build ships, in `SoundId::all()` order (`BpAudioFeed::sound_ids`).
    pub fn all_stems() -> Vec<&'static str> { SoundId::all().into_iter().map(stem).collect() }
    /// The id of a stem, for the few places a stem comes back from GDScript.
    pub fn from_stem(stem: &str) -> Option<SoundId> { SoundId::all().into_iter().find(|id| id.spec().stem == stem) }
    /// The engine loop class of a catalogue engine (`audio` has no workspace dependencies, so it mirrors the
    /// enum). Unit `gd-audio-feed` matches `content::part::EngineKind` onto this.
    pub use audio::mix::EngineClass;
}

// ---------------------------------------------------------------------------------------------------------
// Generated files
// ---------------------------------------------------------------------------------------------------------

/// Paths of everything `cargo xtask gen` writes, relative to the Godot project root. The constants are the
/// writers' own (`hull_mesh::gen_files`, `audio::GEN_DIR`), so a renamed file is a compile error on both
/// sides. DESIGN.md L1076: the folders are `shaders/gen`, `art/gen` and `audio/gen`.
///
/// Textures are raw [`gen::TexBlob`] `.bin` files, not imported resources: read them with
/// `convert::load_tex_blob` and build Godot images with `convert::tex_blob_images`. The wave include is
/// pulled in by `res://shaders/water.gdshader` with `#include`. WAVs import normally.
pub mod gen {
    pub use audio::GEN_DIR as AUDIO_GEN_DIR;
    pub use hull_mesh::gen_files::{
        lut_path, ramp_path, TexBlob, TexFormat, TexKind, LUT_EDGE, LUT_EDGE_HIGH, NOISE_CLOUD_SHADOW, NOISE_WEATHERING,
        OCEAN_DETAIL_FRAMES, OCEAN_DETAIL_LOOP_S, OCEAN_DETAIL_NORMALS, OCEAN_DETAIL_SCALES_M, OCEAN_DETAIL_SIZE,
        OCEAN_DETAIL_TURBULENCE, WAVES_INCLUDE,
    };

    /// `res://` path of a generated file given its project-relative path (one of the constants above, or the
    /// result of `lut_path` or `ramp_path`).
    pub fn res(relative: &str) -> String { format!("res://{relative}") }
    /// `res://audio/gen/<stem>.wav`.
    pub fn sound_res(id: audio::SoundId) -> String { id.res_path() }
}
