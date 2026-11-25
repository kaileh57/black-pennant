//! Part of unit `gd-session`. `BpSessionCheck`: a test-only driver for headless sortie scripts
//! (`game/test/unit/sortie_test.gd`), in the pattern of `BpWorldCheck`. It sends a few player commands
//! through the live link and reads plain facts back from `Services::feed`, so a script can prove a sortie
//! runs end to end (course followed, guns fired, contacts tracked) before `Orders` and `Hud` exist. The game
//! itself never uses it: players command through `Orders`.
use crate::seams::ClientMsg;
use crate::services::services;
use glam::{DVec2, DVec3};
use godot::prelude::*;
use sim_core::events::PhysEvent;
use sim_systems::protocol::event::SimEvent;
use sim_systems::protocol::{Command, ControlRole, Relevance, ShotDesignation};

/// Counters a test reads after a run.
#[derive(Default)]
struct Tally { own_design: Option<content::ids::ContentHash>, mount_fired: u64, own_mount_fired: u64, projectiles: u64, own_projectiles: u64, rejected: u64, events: u64, tracks_created: u64, reveals: u64, kinds: std::collections::BTreeMap<&'static str, u64> }

#[derive(GodotClass)]
#[class(base = RefCounted, init)]
pub struct BpSessionCheck {
    tally: Tally,
    base: Base<RefCounted>,
}

fn send(command: Command) -> bool {
    let services = services();
    let mut c = services.connection.borrow_mut();
    match c.as_mut() { Some(conn) => { conn.link.send(ClientMsg::Command(command)); true } None => false }
}

fn kind_name(e: &SimEvent) -> &'static str {
    match e {
        SimEvent::SortieStarted { .. } => "sortie_started",
        SimEvent::PacingChanged(_) => "pacing_changed",
        SimEvent::EntityRevealed(_) => "entity_revealed",
        SimEvent::EntityHidden { .. } => "entity_hidden",
        SimEvent::Phys(p) => match p {
            PhysEvent::MountFired { .. } => "mount_fired",
            PhysEvent::ProjectileSpawned(_) => "projectile_spawned",
            PhysEvent::Splash { .. } => "splash",
            PhysEvent::ShellImpact { .. } => "shell_impact",
            PhysEvent::WeatherChanged { .. } => "weather_changed",
            PhysEvent::CellsDamaged { .. } => "cells_damaged",
            PhysEvent::FireStarted { .. } => "fire_started",
            PhysEvent::Breach { .. } => "breach",
            PhysEvent::CompartmentHalfFlooded { .. } => "half_flooded",
            PhysEvent::VesselLost { .. } => "vessel_lost",
            PhysEvent::StatusChanged { .. } => "status_changed",
            PhysEvent::BodySpawned { .. } => "body_spawned",
            PhysEvent::BodyImpact { .. } => "body_impact",
            PhysEvent::DecoyDeployed { .. } => "decoy_deployed",
            PhysEvent::Split { .. } => "split",
            PhysEvent::Collision { .. } => "collision",
            _ => "phys_other",
        },
        SimEvent::TrackCreated { .. } => "track_created",
        SimEvent::RungChanged { .. } | SimEvent::StanceChanged { .. } => "track_identity",
        SimEvent::Engaging { .. } => "engaging",
        SimEvent::Holding { .. } => "holding",
        SimEvent::OfficerMessage { .. } => "officer_message",
        SimEvent::ControlChanged { .. } => "control_changed",
        SimEvent::CommandRejected { .. } => "command_rejected",
        SimEvent::AlertRaised(_) | SimEvent::AlertChanged { .. } | SimEvent::AlertEscalated(_) => "alert",
        SimEvent::SortieEnded(_) => "sortie_ended",
        _ => "other",
    }
}

#[godot_api]
impl BpSessionCheck {
    /// `Command::SetCourse` through one map point (x, z metres), at `speed_kn` (negative keeps the speed).
    #[func]
    fn set_course(&mut self, to: Vector2, speed_kn: f64) -> bool {
        if !(to.x.is_finite() && to.y.is_finite()) { return false; }
        send(Command::SetCourse { waypoints: vec![DVec2::new(to.x as f64, to.y as f64)], speed_kn: (speed_kn >= 0.0).then_some(speed_kn), append: false })
    }
    /// `Command::Designate` (`track <= 0` clears).
    #[func]
    fn designate(&mut self, track: i64) -> bool { send(Command::Designate { track: crate::convert::track_from_gd(track) }) }
    /// Take or release one mount by hand.
    #[func]
    fn take_mount(&mut self, slot: i64, on: bool) -> bool {
        let Some(slot) = crate::convert::slot_from_gd(slot) else { return false };
        send(Command::TakeControl { role: ControlRole::Mount(slot), on })
    }
    /// A manual shot from a held mount: at a track when `track > 0`, else along the bow, 2 degrees up.
    #[func]
    fn fire_mount(&mut self, slot: i64, track: i64) -> bool {
        let Some(slot) = crate::convert::slot_from_gd(slot) else { return false };
        let shot = match crate::convert::track_from_gd(track) {
            Some(t) => ShotDesignation::Track { track: t, offset_m: DVec3::ZERO },
            // tan(2 degrees): a shot along the bow, a little above the horizon.
            None => ShotDesignation::Direction { dir_local: DVec3::new(1.0, 0.034_920_8, 0.0).normalize() },
        };
        send(Command::Fire { role: ControlRole::Mount(slot), shot })
    }
    /// End the running sortie on this machine's host thread with `outcome` (`Bp.OUTCOME_*`), as a quit does.
    #[func]
    fn stop_sortie(&mut self, outcome: i64) -> bool {
        use crate::enums::GdEnum;
        let Some(outcome) = sim_systems::protocol::Outcome::from_gd(outcome) else { return false };
        let services = services();
        let c = services.connection.borrow();
        match c.as_ref().and_then(|c| c.host.as_ref()) { Some(h) => { h.send(crate::session::host::HostControl::StopSortie(outcome)); true } None => false }
    }
    /// Count this frame's events (call once per frame, after `Session` has processed).
    #[func]
    fn observe(&mut self) {
        let services = services();
        let feed = services.feed.borrow();
        let own = feed.snapshot.as_ref().and_then(|s| s.ship.as_ref().map(|sh| sh.vessel));
        for e in &feed.events {
            self.tally.events += 1;
            *self.tally.kinds.entry(kind_name(&e.event)).or_default() += 1;
            match &e.event {
                SimEvent::Phys(PhysEvent::MountFired { vessel, .. }) => { self.tally.mount_fired += 1; if Some(*vessel) == own { self.tally.own_mount_fired += 1; } }
                SimEvent::Phys(PhysEvent::ProjectileSpawned(p)) => { self.tally.projectiles += 1; if Some(p.shooter) == own { self.tally.own_projectiles += 1; } }
                SimEvent::CommandRejected { .. } => self.tally.rejected += 1,
                SimEvent::TrackCreated { .. } => self.tally.tracks_created += 1,
                SimEvent::EntityRevealed(i) => {
                    self.tally.reveals += 1;
                    if i.player == Some(content::ids::PlayerSlot(0)) { self.tally.own_design = Some(i.blueprint); }
                }
                _ => {}
            }
        }
    }
    /// Facts of the newest sampled snapshot and the counters so far: `tick: int`, `own: int` (entity or -1),
    /// `pos: Vector2`, `heading_deg`, `speed_kn`, `ordered_heading_deg`, `ordered_speed_kn`, `route: int`
    /// (waypoints), `designated: int`, `tracks: int`, `vessels: int`, `mounts: PackedInt32Array` (own mount
    /// slots), `guns: PackedInt32Array` (own gun mounts, from the design drawn for the own hull), `held: int` (roles held), `shells: int` (client-flown now), `extrapolated: float`, and the
    /// counters `events`, `mount_fired`, `own_mount_fired`, `projectiles`, `own_projectiles`, `rejected`,
    /// `tracks_created`, `reveals`, `kinds: Dictionary` (event kind name to count), `designs: int` (designs
    /// the renderer can draw).
    #[func]
    fn facts(&self) -> VarDictionary {
        let services = services();
        let feed = services.feed.borrow();
        let view = services.view.borrow();
        let mut d = VarDictionary::new();
        let t = &self.tally;
        let mut kinds = VarDictionary::new();
        for (k, n) in &t.kinds { kinds.set(*k, *n as i64); }
        d.set("events", t.events as i64);
        d.set("mount_fired", t.mount_fired as i64);
        d.set("own_mount_fired", t.own_mount_fired as i64);
        d.set("projectiles", t.projectiles as i64);
        d.set("own_projectiles", t.own_projectiles as i64);
        d.set("rejected", t.rejected as i64);
        d.set("tracks_created", t.tracks_created as i64);
        d.set("reveals", t.reveals as i64);
        d.set("kinds", &kinds);
        d.set("designs", services.designs.borrow().len() as i64);
        d.set("shells", view.shells.len() as i64);
        let guns: PackedInt32Array = t.own_design.and_then(|h| match services.designs.borrow().get(&h) {
            Some(crate::worker::ReceivedDesign::Full { design, .. }) => Some(design.mounts.iter().filter(|m| m.weapon == content::graph::WeaponClass::Gun).map(|m| m.slot.0 as i32).collect()),
            _ => None,
        }).unwrap_or_default();
        d.set("guns", &guns);
        d.set("extrapolated", feed.status.extrapolated() as f64);
        let Some(s) = feed.snapshot.as_ref() else { d.set("tick", -1i64); d.set("own", -1i64); return d };
        d.set("tick", s.tick.min(i64::MAX as u64) as i64);
        d.set("tracks", s.tracks.len() as i64);
        d.set("vessels", s.vessels.len() as i64);
        let own = s.vessels.iter().find(|v| v.relevance == Relevance::Own);
        d.set("own", own.map_or(-1, |v| crate::convert::entity_to_gd(v.entity)));
        if let Some(v) = own { d.set("pos", crate::convert::map_from_world(v.kin.pose.pos)); }
        if let Some(ship) = &s.ship {
            d.set("heading_deg", crate::convert::heading_deg(ship.heading_rad as f64));
            d.set("speed_kn", ship.speed_kn as f64);
            d.set("ordered_heading_deg", crate::convert::heading_deg(ship.ordered_heading_rad as f64));
            d.set("ordered_speed_kn", ship.ordered_speed_kn as f64);
            d.set("route", ship.route.len() as i64);
            d.set("designated", crate::convert::track_to_gd(ship.designated));
            let mounts: PackedInt32Array = ship.mounts.iter().map(|m| m.slot.0 as i32).collect();
            d.set("mounts", &mounts);
            d.set("held", ship.held.len() as i64);
        }
        d
    }
    /// Track ids of the newest snapshot, nearest first (from the own hull).
    #[func]
    fn tracks(&self) -> PackedInt64Array {
        let services = services();
        let feed = services.feed.borrow();
        let Some(s) = feed.snapshot.as_ref() else { return PackedInt64Array::new() };
        let own = s.vessels.iter().find(|v| v.relevance == Relevance::Own).map(|v| v.kin.pose.pos).unwrap_or(DVec3::ZERO);
        let mut t: Vec<(f64, i64)> = s.tracks.iter().filter(|t| !t.last_known).map(|t| ((t.pos - own).length(), crate::convert::track_to_gd(t.id))).collect();
        t.sort_by(|a, b| a.0.total_cmp(&b.0));
        t.into_iter().map(|x| x.1).collect()
    }
}
