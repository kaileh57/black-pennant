//! Contract-owned (no work unit). The main-thread service registry: the few pieces of state that more than
//! one facade class reads. DESIGN.md L1088 (data exchange), L1092 (no gameplay logic in Godot nodes).
//!
//! Rules:
//! - Main thread only. `Services` is reached through [`services`], which hands out an `Rc`; it is never sent
//!   to another thread and holds no Godot object.
//! - Each field has exactly one writer (named on the field). Everyone else reads. Borrows are short: take
//!   what you need, drop the borrow before emitting a Godot signal or calling into another facade, because a
//!   signal handler may call back into Rust.
//! - Facades hand work to each other by leaving plain data here (a pending sortie, a finished result, a
//!   tutorial signal), never by calling each other's methods.
//! - Construction is lazy and free of side effects: nothing loads content, opens a file or starts a thread
//!   until a `#[func]` asks. That keeps `init` and `ready` of every autoload inert.
use crate::audio_feed::AudioQueue;
use crate::editor::CurrentDesign;
use crate::interp::{CameraState, ViewFrame};
use crate::quality::QualityProfile;
use crate::session::{Connection, FrameFeed, Handoff};
use crate::settings::ClientSettings;
use crate::worker::WorkerHandle;
use content::campaign::CampaignData;
use content::catalogue::Catalogue;
use sim_systems::sim::SimDeps;
use std::cell::{Cell, OnceCell, RefCell};
use std::rc::Rc;
use std::sync::Arc;

#[derive(Default)]
pub struct Services {
    catalogue: OnceCell<Arc<Catalogue>>,
    campaign: OnceCell<Arc<CampaignData>>,
    /// Client settings (`user://settings.cfg`). Writer: `BpSettings`.
    pub settings: RefCell<ClientSettings>,
    /// Effective graphics profile after auto-detect, overrides, dynamic resolution and thermal state.
    /// Writer: `BpQuality`.
    pub quality: RefCell<QualityProfile>,
    /// The live link and, when this process hosts, the host thread. Writers: `BpSession` (offline sessions,
    /// replays), `BpLobby` (host and join); `BpSession` alone polls it.
    pub connection: RefCell<Option<Connection>>,
    /// Work handed between facades: a sortie to start, a finished sortie to collect.
    /// Writers: whoever hands over; the receiver takes the value out.
    pub handoff: RefCell<Handoff>,
    /// What arrived this frame. Writer: `BpSession::process`, before every other node runs.
    pub feed: RefCell<FrameFeed>,
    /// Interpolated poses for this frame in render space. Writer: `BpSession::process` through [`crate::interp`].
    pub view: RefCell<ViewFrame>,
    /// The active 3D camera (sortie or dry dock). Writers: `BpWorldView`, `BpEditorView`.
    pub camera: Cell<CameraState>,
    /// The hand-aimed direction for the mount or group the player holds: a unit vector from that mount to
    /// the point under the reticle, in the vessel-local frame (+X bow, +Y up, +Z starboard), or all zero
    /// when no weapon role is held or the view has no camera. This is `PlayerInput::aim_local` and the
    /// `dir_local` of `ShotDesignation::Direction`: the simulation lays the gun on what this ray reaches
    /// from the muzzle, so it is taken from the mount, not from the camera. Writer: `BpWorldView`, every
    /// frame of a sortie. Readers: `BpOrders` (`fire_at_reticle`, the trigger) and `BpSession` (the 30 Hz
    /// input).
    pub aim_local: Cell<[f32; 3]>,
    /// Sounds queued this frame and the current loop set. Writers: any unit pushes; `BpAudioFeed` drains.
    pub audio: RefCell<AudioQueue>,
    /// The background worker, started on first use through [`Services::with_worker`].
    pub worker: RefCell<Option<WorkerHandle>>,
    /// The design last saved in the dry dock, with its built form: what Splash launches, what the lobby
    /// fields, what a refit commits. Writer: `BpEditor`.
    pub design: RefCell<Option<Arc<CurrentDesign>>>,
    /// Every design the 3D renderer may be asked to draw in this session, by the hash
    /// `EntityIntro::blueprint` names: the viewer's own fielded designs (full form), stock classes resolved
    /// locally, and each `worker::JobResult::BuiltReceived`. The render units look a revealed entity's hash
    /// up here every frame until it is found, so an entry may arrive after the reveal.
    /// Writer: `BpSession` (inserts when it starts or joins a sortie and on `BuiltReceived`; clears on
    /// `leave`). `BpTestFeed` and `BpBenchmark` insert the designs their scripts reveal.
    pub designs: RefCell<std::collections::BTreeMap<content::ids::ContentHash, crate::worker::ReceivedDesign>>,
    /// Tutorial signals raised by Rust facades this frame (gauge passed, kit placed, guns fired).
    /// Writers: any unit pushes; `BpTutorial` drains.
    pub tutorial_signals: RefCell<Vec<sim_systems::tutorial::Signal>>,
    /// Marked spots for the lesson kits while a tutorial step waits on kits (empty otherwise).
    /// Writer: `BpTutorial`; reader: `BpEditor` (a tutorial dock settles those kits on their spots).
    pub tutorial_spots: RefCell<Vec<sim_systems::tutorial::KitSpot>>,
}

thread_local! {
    static SERVICES: Rc<Services> = Rc::new(Services::default());
}

/// The main thread's registry. Calling it from any other thread yields a separate, empty registry, which is
/// always a bug; debug builds may assert on the thread id.
pub fn services() -> Rc<Services> { SERVICES.with(|s| s.clone()) }

impl Services {
    /// The part catalogue, shared with the simulation threads. Loads the embedded data on first call.
    pub fn catalogue(&self) -> Arc<Catalogue> { self.catalogue.get_or_init(|| Arc::new(content::catalogue::catalogue().clone())).clone() }
    /// Campaign data (theatre, classes, operations, aces, cards, difficulty presets), loaded on first call, with
    /// only the act rows this build ships (`content::campaign::load::shipped_campaign`: Act 1 in 0.1.0).
    pub fn campaign(&self) -> Arc<CampaignData> { self.campaign.get_or_init(|| Arc::new(content::campaign::load::shipped_campaign().clone())).clone() }
    /// Fresh simulation dependencies with default budgets, an empty design cache and the build's script host
    /// (`script::new_host`: the Luau host when built with the `luau` feature, else the null host).
    pub fn sim_deps(&self) -> SimDeps {
        SimDeps {
            catalogue: self.catalogue(),
            campaign: self.campaign(),
            designs: Default::default(),
            scripts: sim_systems::script::new_host(),
            guest_gate_passed: false,
            budgets: Default::default(),
            ladder: Default::default(),
        }
    }
    /// A `Send` factory of the same, for the host thread and the background worker.
    pub fn sim_deps_factory(&self) -> Box<dyn Fn() -> SimDeps + Send> {
        let (catalogue, campaign) = (self.catalogue(), self.campaign());
        Box::new(move || SimDeps {
            catalogue: catalogue.clone(),
            campaign: campaign.clone(),
            designs: Default::default(),
            scripts: sim_systems::script::new_host(),
            guest_gate_passed: false,
            budgets: Default::default(),
            ladder: Default::default(),
        })
    }
    /// Run `f` with the background worker, starting it on first use.
    pub fn with_worker<R>(&self, f: impl FnOnce(&WorkerHandle) -> R) -> R {
        let mut slot = self.worker.borrow_mut();
        let worker = slot.get_or_insert_with(|| WorkerHandle::spawn(self.sim_deps_factory()));
        f(worker)
    }
    /// Push one tutorial signal (no-op cost when no tutorial runs: the queue is cleared each frame).
    pub fn tutorial_signal(&self, signal: sim_systems::tutorial::Signal) { self.tutorial_signals.borrow_mut().push(signal); }
}
