//! Unit `gd-testfeed`. `BpTestFeed`: the test-only entry point that feeds scripted snapshots and events into
//! the real client, so GdUnit4 suites exercise UI glue against the real library with no mocks.
//! DESIGN.md L1304 (tests load the real gdclient library and feed it scripted snapshots through a test-only
//! Rust entry point), L1749-1755 (capture harness: every screen with real data), L1616 (toast check on a
//! scripted 20 minute battle), L1764 (declutter gate on the reference battle with 41 hulls).
//! A script is a named, deterministic generator of `Snapshot`s and `WireEvent`s built in Rust
//! (`scripts()` lists them). `start` installs it as the session's link (`Bp.KIND_SCRIPTED`): a `ClientLink`
//! that pushes the script's output through a `link::channel::Presenter` on a hand-driven clock, so `Hud`,
//! `BpPlotView`, `BpWorldView` and `AudioFeed` receive the same sampled frames as in play. Not referenced by
//! shipped scenes.
//!
//! # Using it
//! `start(name)` builds the script (a real one-hull `Sim` for the own ship's template, see [`world`]) and
//! installs the link; nothing moves until `advance`. `advance(n)` generates `n` ticks and queues their output;
//! `BpSession` delivers it on its next frame, so a test awaits one `process_frame` after `advance` before
//! reading `Hud`, `Session` or the views. The first `advance` of at least 10 ticks gives the first sampled
//! frame and moves the session to `Bp.SESSION_IN_SORTIE`. The link's clock is the script's tick (arrival and
//! sampling at `tick * TICK_NS`), so wall time never matters: the sampled frame is the newest tick minus the
//! interpolation delay (8 ticks), with no extrapolation.
//!
//! # Decisions where the contract is silent
//! - The link is a single player's (`NetRole::Offline`), except `coop_host_loss`, which is a joined member
//!   (`NetRole::Client`, slot 1, a `Coop::Member`) so host silence and host loss take the client path.
//! - `take_commands` keeps at most [`MAX_COMMANDS`] commands between calls (the oldest go); the 30 Hz input
//!   is not a command and is not captured, though the helm takeover reads it.
//! - `stop` ends the scripted session through `Session.leave(false)`, which closes the link and returns the
//!   session to idle; when no `Session` autoload exists the connection is dropped directly.
//! - `advance` takes at most [`MAX_ADVANCE`] ticks per call (an hour of sortie time); larger or negative
//!   values are clamped.
use crate::enums::{GdEnum, NetRole, SessionKind, SessionState};
use crate::link::channel::Presenter;
use content::ids::ContentHash;
use crate::seams::{ClientEvent, ClientLink, ClientMsg, InterpTuning, LinkInbox, LinkState, LinkStatus, SessionEnd, WireEvent};
use crate::services::services;
use godot::prelude::*;
use sim_systems::protocol::Snapshot;
use std::cell::RefCell;
use std::collections::VecDeque;
use std::rc::Rc;
use std::time::Duration;

mod world;

/// Commands kept for `take_commands` between calls.
pub(crate) const MAX_COMMANDS: usize = 1_024;
/// Most ticks one `advance` generates: an hour of sortie time.
pub(crate) const MAX_ADVANCE: i64 = 60 * 60 * world::TICK_HZ as i64;

/// Scripted session feed for tests and captures. Instantiate with `BpTestFeed.new()`; free it after use.
#[derive(GodotClass)]
#[class(base = Node)]
pub struct BpTestFeed {
    pub(crate) script: String,
    pub(crate) tick: u64,
    /// The running script, shared with the link the session holds.
    pub(crate) shared: Option<Rc<RefCell<Feed>>>,
    base: Base<Node>,
}

#[godot_api]
impl INode for BpTestFeed {
    fn init(base: Base<Node>) -> Self { Self { script: String::new(), tick: 0, shared: None, base } }
}

#[godot_api]
impl BpTestFeed {
    /// Names of the built-in scripts: at least `"calm"` (own hull, no contacts, no alerts: the 30 s calm
    /// capture), `"plot_40_tracks"` (golden scene 2), `"battle"` (fire, flooding, alerts of every
    /// priority, officer lines), `"alert_flood"`, `"track_events"` (create, merge, split, rung and stance
    /// changes), `"coop_host_loss"`, `"reference_battle"` (41 hulls and missiles for the declutter gate).
    #[func]
    fn scripts(&self) -> PackedStringArray { world::SCRIPTS.iter().map(|s| GString::from(*s)).collect() }
    /// Install a script as the live session. Returns false for an unknown name or while a session is live.
    #[func]
    fn start(&mut self, script: GString) -> bool {
        let name = script.to_string();
        let Some(kind) = world::Kind::from_name(&name) else {
            godot_warn!("testfeed: no script named {name}");
            return false;
        };
        let services = services();
        // The session's own state, not this frame's feed copy: `stop` then `start` in one frame must work.
        let state = autoload("Session").and_then(|mut s| s.try_call("state", &[]).ok()).and_then(|v| v.try_to::<i64>().ok()).and_then(SessionState::from_gd).unwrap_or(services.feed.borrow().state);
        let live = services.connection.borrow().is_some() || state != SessionState::Idle;
        if live || services.handoff.borrow().sortie.is_some() {
            godot_warn!("testfeed: a session is live; leave it first");
            return false;
        }
        let template = match world::build_template(kind, &services) {
            Ok(t) => t,
            Err(e) => { godot_warn!("testfeed: script {name} could not be built: {e}"); return false; }
        };
        let mut w = world::World::new(kind, template, &services.campaign(), &services.catalogue());
        if let Some((hash, design)) = w.take_design() { services.designs.borrow_mut().insert(hash, design); }
        let feed = Rc::new(RefCell::new(Feed::new(w, services.catalogue())));
        let coop = kind.is_client().then(|| crate::session::Coop::Member(net::coop::CoopMember::new(0x0b1a_c4e0)));
        let net = if kind.is_client() { NetRole::Client } else { NetRole::Offline };
        *services.connection.borrow_mut() = Some(crate::session::Connection {
            link: Box::new(ScriptedLink(feed.clone())), host: None, hosting: None, welcome: None, intro: None, coop, kind: SessionKind::Scripted, net,
        });
        self.shared = Some(feed);
        self.script = name;
        self.tick = 0;
        true
    }
    /// Advance the script and its clock by this many simulation ticks and deliver what it produced, as one
    /// frame would (the sampled frame trails the script by the interpolation delay, 8 ticks by default).
    /// Tests call this instead of waiting on the wall clock.
    #[func]
    fn advance(&mut self, ticks: i64) {
        let n = ticks.clamp(0, MAX_ADVANCE) as u64;
        if n == 0 { return; }
        let Some(feed) = &self.shared else { return };
        feed.borrow_mut().advance(n);
        self.tick += n;
    }
    /// End the scripted session and return the session to idle.
    #[func]
    fn stop(&mut self) {
        let Some(feed) = self.shared.take() else { return };
        feed.borrow_mut().close();
        let ours = services().connection.borrow().as_ref().is_some_and(|c| c.kind == SessionKind::Scripted);
        if !ours { return; }
        let left = autoload("Session").is_some_and(|mut session| session.try_call("leave", &[false.to_variant()]).is_ok());
        if !left { let conn = services().connection.borrow_mut().take(); drop(conn); }
    }
    /// Commands the UI sent since the last call, as text (`Debug` form of `protocol::Command`), so a test can
    /// assert that a click produced the right order.
    #[func]
    fn take_commands(&mut self) -> PackedStringArray {
        let Some(feed) = &self.shared else { return PackedStringArray::new() };
        let list: Vec<String> = feed.borrow_mut().commands.drain(..).collect();
        list.iter().map(|s| GString::from(s.as_str())).collect()
    }
}

/// An autoload by name, from the running scene tree (this node need not be in the tree).
fn autoload(name: &str) -> Option<Gd<Node>> {
    let tree = godot::classes::Engine::singleton().get_main_loop()?.try_cast::<godot::classes::SceneTree>().ok()?;
    let root: Gd<Node> = tree.get_root().upcast();
    root.get_node_or_null(name)
}

/// The running script and what it produced but the session has not polled yet.
pub(crate) struct Feed {
    world: world::World,
    presenter: Presenter,
    session: Vec<ClientEvent>,
    events: Vec<WireEvent>,
    snapshots: Vec<Snapshot>,
    /// Newest generated tick + 1: the link's clock reads `now_tick * TICK_NS`.
    now_tick: u64,
    state: LinkState,
    pub(crate) commands: VecDeque<String>,
    /// The session ends (host loss) once its pending events are delivered.
    ending: Option<SessionEnd>,
}

impl Feed {
    fn new(world: world::World, catalogue: std::sync::Arc<content::catalogue::Catalogue>) -> Feed {
        let session = world.opening_session();
        Feed { world, presenter: Presenter::new(InterpTuning::default(), catalogue), session, events: Vec::new(), snapshots: Vec::new(), now_tick: 0, state: LinkState::Joined, commands: VecDeque::new(), ending: None }
    }
    fn advance(&mut self, n: u64) {
        if self.state != LinkState::Joined || self.ending.is_some() { return; }
        let mut out = world::Produced::default();
        self.world.advance(n, &mut out);
        self.events.append(&mut out.events);
        self.session.append(&mut out.session);
        self.snapshots.append(&mut out.snapshots);
        self.now_tick = self.world.link_tick;
        if let Some(end) = out.ended { self.ending = Some(end); }
    }
    fn close(&mut self) {
        self.state = LinkState::Closed;
        self.presenter.clear();
        self.events.clear();
        self.snapshots.clear();
        self.session.clear();
    }
    /// The link clock at a tick: the arrival time of that tick's snapshot.
    fn at(tick: u64) -> Duration { Duration::from_nanos(tick.saturating_mul(crate::session::host::TICK_NS)) }

    fn poll(&mut self, inbox: &mut LinkInbox) {
        if self.state == LinkState::Closed { return; }
        inbox.session.append(&mut self.session);
        if let Some(end) = self.ending.take() {
            // Host loss: what arrived before it is delivered, then the link is over.
            self.deliver(inbox);
            self.state = LinkState::Ended(end);
            self.presenter.clear();
            return;
        }
        if !matches!(self.state, LinkState::Joined) { return; }
        self.deliver(inbox);
    }
    fn deliver(&mut self, inbox: &mut LinkInbox) {
        // Events before snapshots: reveals gate the poses they introduce.
        for e in self.events.drain(..) {
            self.presenter.push_event(&e);
            inbox.events.push(e);
        }
        for s in self.snapshots.drain(..) {
            let at = Self::at(s.tick);
            self.presenter.push_snapshot(s, at);
        }
        if self.now_tick > 0 { self.presenter.sample(Self::at(self.now_tick - 1), inbox); }
    }
    fn send(&mut self, msg: ClientMsg) {
        if self.state != LinkState::Joined { return; }
        match msg {
            ClientMsg::Command(c) => {
                if self.commands.len() >= MAX_COMMANDS { self.commands.pop_front(); }
                self.commands.push_back(format!("{c:?}"));
                self.world.apply(&c);
            }
            ClientMsg::Input(i) => self.world.input(i),
            _ => {}
        }
    }
}

/// The `ClientLink` the session holds: the same `Feed` as the node.
struct ScriptedLink(Rc<RefCell<Feed>>);
impl ClientLink for ScriptedLink {
    fn poll(&mut self, _now_s: f64, inbox: &mut LinkInbox) { self.0.borrow_mut().poll(inbox) }
    fn send(&mut self, msg: ClientMsg) { self.0.borrow_mut().send(msg) }
    fn status(&self) -> LinkStatus { let f = self.0.borrow(); LinkStatus { state: f.state.clone(), link: Default::default(), interp: f.presenter.stats() } }
    fn blueprint(&mut self, hash: &ContentHash) -> Option<content::share::PackedBlueprint> {
        self.0.borrow().world.packed().filter(|p| p.hash == *hash).cloned()
    }
    fn close(&mut self) { self.0.borrow_mut().close() }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn feed(kind: world::Kind) -> Feed {
        let campaign = content::campaign::load::campaign();
        let w = world::World::new(kind, world::tests::bare_template(), campaign, content::catalogue::catalogue());
        Feed::new(w, std::sync::Arc::new(content::catalogue::catalogue().clone()))
    }

    #[test]
    fn the_sampled_frame_trails_the_newest_tick_by_the_delay_without_extrapolating() {
        let mut f = feed(world::Kind::Calm);
        let mut inbox = LinkInbox::default();
        f.poll(&mut inbox);
        assert!(matches!(inbox.session.first(), Some(ClientEvent::Welcomed(_))));
        assert!(matches!(inbox.session.get(1), Some(ClientEvent::SortieLoading(_))));
        assert!(inbox.frame.is_none());
        for _ in 0..40 {
            f.advance(2);
            let mut inbox = LinkInbox::default();
            f.poll(&mut inbox);
        }
        let mut inbox = LinkInbox::default();
        f.advance(2);
        f.poll(&mut inbox);
        let frame = inbox.frame.expect("a sampled frame");
        let newest = f.now_tick - 1;
        assert!(!frame.extrapolating);
        assert!((frame.render_tick - (newest as f64 - 8.0)).abs() <= 1.0, "render tick {} newest {newest}", frame.render_tick);
        assert_eq!(inbox.newest_tick, Some(newest - newest % 2));
    }

    #[test]
    fn commands_are_captured_capped_and_applied_and_input_is_not_captured() {
        let mut f = feed(world::Kind::Calm);
        f.send(ClientMsg::Command(sim_systems::protocol::Command::SetSpeed { speed_kn: 5.0 }));
        f.send(ClientMsg::Input(Default::default()));
        assert_eq!(f.commands.len(), 1);
        assert!(f.commands[0].starts_with("SetSpeed"));
        for _ in 0..(MAX_COMMANDS + 10) { f.send(ClientMsg::Command(sim_systems::protocol::Command::SilenceAlerts)); }
        assert_eq!(f.commands.len(), MAX_COMMANDS);
    }

    #[test]
    fn host_loss_ends_the_link_after_its_events_are_delivered() {
        let mut f = feed(world::Kind::CoopHostLoss);
        f.advance(60 * 45);
        let mut inbox = LinkInbox::default();
        f.poll(&mut inbox);
        assert!(inbox.session.iter().any(|e| matches!(e, ClientEvent::Ended(SessionEnd::HostLost))));
        assert_eq!(f.state, LinkState::Ended(SessionEnd::HostLost));
        // Nothing more comes, and nothing can be sent.
        f.advance(60);
        let mut inbox = LinkInbox::default();
        f.poll(&mut inbox);
        assert!(inbox.frame.is_none() && inbox.events.is_empty());
        f.send(ClientMsg::Command(sim_systems::protocol::Command::SilenceAlerts));
        assert!(f.commands.is_empty());
    }
}
