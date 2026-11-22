//! gdclient: the only crate that depends on Godot (gdext 0.5.5, `api-4-7`). DESIGN.md "Crate layout"
//! (L1060-1074), "Threading and the Godot boundary" (L1078-1097), "Godot implementation" (L1725-1744).
//!
//! What lives here: the Godot-visible facade classes (`Bp*`), the host driver that runs the simulation on Rust
//! threads, snapshot interpolation, and all 3D rendering of simulation state through `RenderingServer`.
//! GDScript is UI glue only and holds no gameplay logic (L1076, L1092).
//!
//! # Boundary rules (L1088-1097)
//! - Godot bindings are touched from the main thread only. `experimental-threads` is never enabled. No type
//!   from the `godot` crate may appear in a struct that crosses to another thread.
//! - The simulation exchanges data with the main thread through snapshots and queues only
//!   ([`seams::ClientLink`] on the main thread, [`seams::HostPort`] on the clock thread, joined by `net`'s
//!   in-memory transport or by `link::channel`'s triple buffer); the adapter's main-thread budget is 2 ms
//!   per frame (L1449, split in L2002-2010).
//! - Types that cross to `net`, `hull-mesh` or `audio` are those crates' own, re-exported once in
//!   [`seams`]. No module mirrors one: no local surface, chunk, quality-tier, sound-id, lobby-message or
//!   close-reason type exists in this crate.
//! - No Godot physics. Picking goes through Rust queries (`BpWorldView::pick`, `BpPlotView`, `BpEditorView::pick`).
//! - `mesh_clear` is never called on a visible chunk (L1929); a CI lint greps this crate for it.
//! - Client-side code may use std maths and wall clocks; it never feeds results back into the simulation.
//!
//! # Threads (L1084-1086)
//! main thread (Godot) | clock thread + sim pool of exactly 3 rayon workers ([`session::host`]) |
//! 1 meshing thread ([`render::hull`]) | 1 background worker ([`worker`]).
//!
//! # Per-frame order on the main thread
//! Autoload nodes process in this order (process priority in brackets), each reading plain data the earlier
//! ones left in [`services::Services`]:
//! 1. `Session` [-100]: polls the link, fills `Services::feed` (this frame's sampled snapshot, events of
//!    this frame), runs [`interp`] and fills `Services::view`.
//! 2. `Hud` [-90], `Campaign` [-90], `Lobby` [-90], `Editor` [-90]: update their models, emit change signals.
//! 3. `Tutorial` [-80], `AudioFeed` [-70], `Bench` [-60], `Quality` [-60].
//! 4. Scene nodes [0]: `BpWorldView` (transforms, MultiMesh buffers, wave uniforms), `BpPlotView`,
//!    `BpEditorView`, then GDScript UI.
//!
//! # GDScript-facing surface
//! Shapes of every Dictionary are in [`shapes`]; enumerations are `Bp.*` constants ([`enums`]). Each `#[func]`
//! doc comment states argument and return shapes. Lifecycle methods (`init`, `ready`, `process`,
//! `exit_tree`) never panic and never touch `content` or `sim-systems` before a `#[func]` asks for it, so the
//! extension loads and the headless smoke test passes while units are still `todo!()`.
//!
//! # Godot classes (autoload name in brackets)
//! | Class | File | Unit |
//! | `Bp` | `enums.rs` | contract |
//! | `BpProbe` | `lib.rs` | contract (smoke test) |
//! | `BpApp` [App] | `app.rs` | `gd-app` |
//! | `BpSettings` [Settings] | `settings.rs` | `gd-settings` |
//! | `BpQuality` [Quality] | `quality.rs` | `gd-quality` |
//! | `BpText` [Tx] | `text.rs` | `gd-text` |
//! | `BpContent` [Content] | `content_api.rs` | `gd-content` |
//! | `BpSession` [Session] | `session/mod.rs` | `gd-session` |
//! | `BpOrders` [Orders] | `orders.rs` | `gd-orders` |
//! | `BpHud` [Hud] | `hud/*.rs` | `gd-hud-core`, `gd-hud-ship`, `gd-hud-tracks`, `gd-hud-alerts`, `gd-hud-board`, `gd-hud-damage` |
//! | `BpEditor` [Editor] | `editor/*.rs` | `gd-editor-core`, `-hull`, `-parts`, `-control`, `-graph`, `-analysis`, `-io`, `-splash` |
//! | `BpEditorView` | `editor/view.rs` | `gd-editor-view` |
//! | `BpCampaign` [Campaign] | `campaign/*.rs` | `gd-campaign`, `gd-campaign-saves` |
//! | `BpLobby` [Lobby] | `lobby.rs` | `gd-lobby` |
//! | `BpTutorial` [Tutorial] | `tutorial.rs` | `gd-tutorial` |
//! | `BpAudioFeed` [AudioFeed] | `audio_feed.rs` | `gd-audio-feed` |
//! | `BpBenchmark` [Bench] | `benchmark.rs` | `gd-benchmark` |
//! | `BpWorldView` | `world/mod.rs` | `gd-world-view` |
//! | `BpPlotView` | `plot/mod.rs` | `gd-plot` |
//! | `BpTestFeed` | `testfeed.rs` | `gd-testfeed` |
//!
//! Rust-only units: `gd-convert` (`convert.rs`), `gd-platform` (`platform.rs`), `gd-host` (`session/host.rs`), `gd-link-channel`,
//! `gd-link-net`, `gd-link-replay` (`link/`), `gd-interp` (`interp.rs`), `gd-worker` (`worker.rs`),
//! `gd-render-hull`, `-parts`, `-projectiles`, `-vfx`, `-particles`, `-ocean`, `-scenery` (`render/`).
//! Contract-owned files: this file, `enums.rs`, `shapes.rs`, `seams.rs`, `services.rs`, `render/mod.rs`,
//! `link/mod.rs`. In unit-owned files the public types, `#[func]` signatures, signals and doc comments are
//! frozen contract: owners fill `todo!()` bodies and add private helpers or extra files beside their own.
#![allow(dead_code, unused_variables, unused_imports)]
// The steam feature only forwards to net (L1203); nothing here may reference Steam without it.

use godot::prelude::*;

pub mod app;
pub mod audio_feed;
pub mod benchmark;
pub mod campaign;
pub mod content_api;
pub mod convert;
pub mod editor;
pub mod enums;
pub mod hud;
pub mod interp;
pub mod link;
pub mod lobby;
pub mod orders;
pub mod platform;
pub mod plot;
pub mod quality;
pub mod render;
pub mod seams;
pub mod services;
pub mod session;
pub mod settings;
pub mod shapes;
pub mod testfeed;
pub mod text;
pub mod tutorial;
pub mod worker;
pub mod world;

struct BlackPennantExtension;

#[gdextension]
unsafe impl ExtensionLibrary for BlackPennantExtension {
    /// When the main loop starts: register the wave and cloud-shadow global shader parameters and add the
    /// shader clock node to the tree root (`render::ocean::clock`, L1891). Both must exist before any shader
    /// loads and before the first frame is drawn, which no scene node can promise.
    fn on_stage_init(stage: InitStage) {
        if stage == InitStage::MainLoop { render::ocean::clock::install(); }
    }
    /// Retries the install above for an engine that had no tree root at that stage; one flag test per frame.
    fn on_main_loop_frame() { render::ocean::clock::ensure_installed(); }
}

/// Smoke-test node proving the extension loads (`game/boot/boot.gd` prints its version).
#[derive(GodotClass)]
#[class(base = Node)]
struct BpProbe {
    base: Base<Node>,
}

#[godot_api]
impl INode for BpProbe {
    fn init(base: Base<Node>) -> Self {
        Self { base }
    }
}

#[godot_api]
impl BpProbe {
    /// Crate version as `String`, for example `"0.1.0"`.
    #[func]
    fn version(&self) -> GString {
        GString::from(env!("CARGO_PKG_VERSION"))
    }
}
