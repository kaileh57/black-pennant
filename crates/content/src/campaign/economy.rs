//! Economy, campaign rules and difficulty. DESIGN.md L749-750 (alert, clock), L772 (doctrine), L811-828
//! (economy, failure), L849-860 (difficulty), L92-102 (commitment). Gate fallbacks rewrite these numbers,
//! so they are data (`data/campaign_rules.ron`); `Default` carries the spec's starting values.
use crate::ids::{DifficultyId, TextKey};
use serde::{Deserialize, Serialize};

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields, default)]
pub struct CampaignRules {
    // Commitment (L94-102).
    pub hull_multiple: u8,
    /// Default 4, moved within 3..=5 by the success-rate gate. The hull multiple never changes.
    pub tonnage_multiple: f64,
    pub max_capitals: u8,
    // Regional Alert (L749).
    pub alert_observed_kill: f64,
    pub alert_unobserved_kill: f64,
    pub alert_unobserved_delay_h: u32,
    pub alert_sighting: f64,
    pub alert_sighting_cap: f64,
    pub alert_evidence: f64,
    pub alert_evidence_cap: f64,
    pub alert_decay_quiet_h: u32,
    pub alert_escalation: u8,
    pub hunter_warning_h: u32,
    pub retreat_alert: f64,
    pub normal_coves: u8,
    pub retreat_coves: u8,
    // Logistics (L747-748).
    pub supply_window_days: u32,
    pub patrol_floor: f64,
    pub lane_cut_days: u32,
    pub task_group_hulls: u8,
    pub admiral_replan_h: u32,
    // Operation clock (L750).
    pub pushback_template: f64,
    pub pushback_strike: f64,
    pub pushback_ace: f64,
    pub pushback_story: f64,
    // Doctrine (L772).
    pub tally_threshold: f64,
    pub tally_half_life_h: f64,
    pub card_lead_h: u32,
    pub card_delay_h: u32,
    pub max_cards_per_region: u8,
    // Intel prices (L811-822).
    pub intel_delay_card: u8,
    pub intel_cancel_card: u8,
    pub intel_reveal_convoys: u8,
    pub intel_reveal_ace: u8,
    pub reveal_convoys_days: u32,
    // Templates (L782).
    pub template_repeat_cap: u8,
    pub open_theatre_repeat_window_days: u32,
    /// Approach-distance scale per operation set by the length-gate fallback (L85-90): 25% steps within
    /// 0.5..=1.5 of the authored distance. Operations not listed use 1.0.
    pub approach_scale: std::collections::BTreeMap<crate::ids::OperationId, f64>,
    // Economy (L811-822).
    pub wreck_payout: f64,
    pub prize_home_payout: f64,
    pub prize_strip_payout: f64,
    pub repair_cost_fraction: f64,
    pub reload_cost_fraction: f64,
    pub rebuild_cost_fraction: f64,
    pub rebuild_hours: u32,
    pub sextant_loss_fraction: f64,
    pub sextant_refit_hours: u32,
    pub refit_hours_per_module: u32,
    pub refit_tonnes_per_hour: f64,
    // Co-op (L832-845).
    pub rejoin_minutes: u32,
    pub checkpoint_s: u32,
    /// The Combine surrender rule's cells-lost fraction (L537, ruling 1 of 20 November 2026): a hull under fire
    /// strikes its colours once it has lost this share of its original cells. Calibrated to the L610-L612 gates.
    pub strike_cells_lost: f64,
    /// The Act 1 winnability fallback (L317): hostile hulls each listed operation drops from the end of its
    /// generated force, one per failing step. The officers are not changed. Operations not listed drop none.
    pub hostile_drop: std::collections::BTreeMap<crate::ids::OperationId, u8>,
}
impl Default for CampaignRules {
    fn default() -> Self {
        Self {
            hull_multiple: 5, tonnage_multiple: 4.0, max_capitals: 4,
            alert_observed_kill: 1.0, alert_unobserved_kill: 0.5, alert_unobserved_delay_h: 12, alert_sighting: 0.5, alert_sighting_cap: 1.0, alert_evidence: 0.25, alert_evidence_cap: 1.0, alert_decay_quiet_h: 48, alert_escalation: 3, hunter_warning_h: 24, retreat_alert: 2.0, normal_coves: 3, retreat_coves: 2,
            supply_window_days: 4, patrol_floor: 0.25, lane_cut_days: 10, task_group_hulls: 5, admiral_replan_h: 6,
            pushback_template: 1.0, pushback_strike: 2.0, pushback_ace: 2.0, pushback_story: 3.0,
            tally_threshold: 10.0, tally_half_life_h: 120.0, card_lead_h: 72, card_delay_h: 72, max_cards_per_region: 2,
            intel_delay_card: 2, intel_cancel_card: 5, intel_reveal_convoys: 1, intel_reveal_ace: 3, reveal_convoys_days: 4,
            template_repeat_cap: 3, open_theatre_repeat_window_days: 40, approach_scale: std::collections::BTreeMap::new(),
            wreck_payout: 0.20, prize_home_payout: 0.60, prize_strip_payout: 0.40, repair_cost_fraction: 0.50, reload_cost_fraction: 0.05, rebuild_cost_fraction: 0.60, rebuild_hours: 48, sextant_loss_fraction: 0.30, sextant_refit_hours: 72, refit_hours_per_module: 2, refit_tonnes_per_hour: 10.0,
            rejoin_minutes: 5, checkpoint_s: 60,
            strike_cells_lost: 0.006,
            hostile_drop: std::collections::BTreeMap::new(),
        }
    }
}

/// The six difficulty sliders. There is no damage slider (L57, L849-860).
#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct DifficultySliders {
    /// Aim-error multiplier, 0.6..=2.0; 1.0 = stock officers.
    pub ai_accuracy: f64,
    /// Seconds, 0.8..=4.0.
    pub ai_reaction_s: f64,
    /// 0.25..=2.0.
    pub adaptation_cadence: f64,
    /// Persistent Combine hull records, 200..=450. Locks at campaign start.
    pub hull_count: u16,
    /// Salvage income multiplier, 0.7..=1.5.
    pub economy: f64,
    /// Operation clock in theatre days, 28..=60.
    pub clock_days: u16,
}
impl DifficultySliders {
    pub const EASY: Self = Self { ai_accuracy: 1.5, ai_reaction_s: 3.0, adaptation_cadence: 0.5, hull_count: 220, economy: 1.25, clock_days: 50 };
    pub const NORMAL: Self = Self { ai_accuracy: 1.0, ai_reaction_s: 2.0, adaptation_cadence: 1.0, hull_count: 300, economy: 1.0, clock_days: 40 };
    pub const HARD: Self = Self { ai_accuracy: 0.75, ai_reaction_s: 1.2, adaptation_cadence: 1.5, hull_count: 380, economy: 0.85, clock_days: 32 };
    /// Clamp every slider into its legal range (L851-858).
    pub fn clamped(self) -> Self {
        Self { ai_accuracy: self.ai_accuracy.clamp(0.6, 2.0), ai_reaction_s: self.ai_reaction_s.clamp(0.8, 4.0), adaptation_cadence: self.adaptation_cadence.clamp(0.25, 2.0), hull_count: self.hull_count.clamp(200, 450), economy: self.economy.clamp(0.7, 1.5), clock_days: self.clock_days.clamp(28, 60) }
    }
}
impl Default for DifficultySliders { fn default() -> Self { Self::NORMAL } }

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct DifficultyPreset { pub id: DifficultyId, pub name: TextKey, pub sliders: DifficultySliders }
