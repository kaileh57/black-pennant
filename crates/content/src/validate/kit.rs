//! Kit definition checks (DESIGN.md L140, L2087).
use super::graph::check_graph;
use super::Issue;
use crate::catalogue::Catalogue;
use crate::enums::ControlLayer;
use crate::graph::{NodeKind, SinkKind, SourceKind};
use crate::ids::{PartId, PartSlot};
use crate::kit::{rotate_offset, KitDef, KitKind};
use crate::ruleset::{Ruleset, RulesetKind};
use std::collections::{BTreeMap, BTreeSet};

fn bad(detail: impl Into<String>) -> Issue { Issue::BadKitBinding { detail: detail.into() } }

/// Copy limit for one part under a ruleset: the lobby's own limit, else the part's standard limit under the
/// standard ruleset. Open rulesets (and the campaign) have no default limits.
pub(super) fn part_limit(ruleset: &Ruleset, id: &PartId, standard_limit: Option<u16>) -> Option<u16> {
    ruleset.part_limits.get(id).copied().or(if ruleset.kind == RulesetKind::Standard { standard_limit } else { None })
}

/// True when this kit is stock content: the catalogue's definition, unchanged.
pub(super) fn is_stock(kit: &KitDef, catalogue: &Catalogue) -> bool { catalogue.kit(&kit.id).is_some_and(|c| c == kit) }

/// Whether fielding this kit needs graph-layer rights: a player-authored kit that forks an officer or carries
/// its own graph fragment (L2087). Stock kits are stock fire control.
pub(super) fn needs_graph_layer(kit: &KitDef, catalogue: &Catalogue) -> bool {
    !is_stock(kit, catalogue) && (kit.kind == KitKind::Officer || !kit.graph.nodes.is_empty() || !kit.graph.edges.is_empty())
}

/// Part slot a node kind refers to, if any (kit fragments index the kit's own parts).
fn node_slot(kind: &NodeKind) -> Option<PartSlot> {
    match kind {
        NodeKind::Source(SourceKind::PartState(s) | SourceKind::Detections(s)) => Some(*s),
        NodeKind::Sink(k) => match k {
            SinkKind::MountAimPoint(s) | SinkKind::MountFirePermission(s) | SinkKind::MissileLaunch(s) | SinkKind::MissileTarget(s) | SinkKind::EmissionState(s) | SinkKind::PowerPriority(s) | SinkKind::Pump(s) | SinkKind::Door(s) => Some(*s),
            _ => None,
        },
        _ => None,
    }
}

pub(super) fn validate_kit(kit: &KitDef, catalogue: &Catalogue, ruleset: &Ruleset) -> Vec<Issue> {
    let mut issues = Vec::new();
    match kit.kind {
        KitKind::Officer if !kit.parts.is_empty() => issues.push(bad(format!("officer kit `{}` carries parts", kit.id))),
        KitKind::Assembly if kit.parts.is_empty() => issues.push(bad(format!("assembly kit `{}` has no parts", kit.id))),
        _ => {}
    }

    // Parts: known, within copy limits, not overlapping each other.
    let mut unknown = BTreeSet::new();
    let mut counts: BTreeMap<&PartId, u16> = BTreeMap::new();
    let mut cells: Vec<([i32; 3], usize)> = Vec::new();
    for (i, kp) in kit.parts.iter().enumerate() {
        let Some(def) = catalogue.part(&kp.part) else {
            unknown.insert(kp.part.clone());
            continue;
        };
        let n = counts.entry(&kp.part).or_insert(0);
        *n = n.saturating_add(1);
        let size = def.size;
        for x in 0..size[0] {
            for y in 0..size[1] {
                for z in 0..size[2] {
                    let o = rotate_offset(crate::cell::CellPos::new(x as i16, y as i16, z as i16), size, kp.rot);
                    cells.push(([kp.offset.x as i32 + o.x as i32, kp.offset.y as i32 + o.y as i32, kp.offset.z as i32 + o.z as i32], i));
                }
            }
        }
    }
    issues.extend(unknown.into_iter().map(|part| Issue::UnknownPart { part }));
    for (id, count) in counts {
        let std_limit = catalogue.part(id).and_then(|d| d.standard_limit);
        if let Some(limit) = part_limit(ruleset, id, std_limit) {
            if count > limit {
                issues.push(Issue::PartOverLimit { part: id.clone(), count, limit });
            }
        }
    }
    cells.sort_unstable();
    let mut overlaps: BTreeMap<usize, usize> = BTreeMap::new();
    for w in cells.windows(2) {
        if w[0].0 == w[1].0 && w[0].1 != w[1].1 {
            let (a, b) = (w[0].1.min(w[1].1), w[0].1.max(w[1].1));
            let e = overlaps.entry(b).or_insert(a);
            *e = (*e).min(a);
        }
    }
    for (b, a) in overlaps {
        issues.push(Issue::PartsOverlap { a: PartSlot(a.min(u16::MAX as usize) as u16), b: PartSlot(b.min(u16::MAX as usize) as u16) });
    }

    // Sockets: named once each, inside the kit's footprint box.
    let (mut lo, mut hi) = ([i32::MAX; 3], [i32::MIN; 3]);
    for (c, _) in &cells {
        for k in 0..3 {
            lo[k] = lo[k].min(c[k]);
            hi[k] = hi[k].max(c[k]);
        }
    }
    if cells.is_empty() {
        (lo, hi) = ([0; 3], [0; 3]);
    }
    let mut names = BTreeSet::new();
    for s in &kit.sockets {
        if s.name.trim().is_empty() {
            issues.push(bad(format!("kit `{}` has a socket with no name", kit.id)));
        } else if !names.insert(s.name.as_str()) {
            issues.push(bad(format!("kit `{}` names socket `{}` twice", kit.id, s.name)));
        }
        let o = [s.offset.x as i32, s.offset.y as i32, s.offset.z as i32];
        if (0..3).any(|k| o[k] < lo[k] || o[k] > hi[k]) {
            issues.push(bad(format!("kit `{}` socket `{}` lies outside the kit's footprint", kit.id, s.name)));
        }
    }

    // Settings sheet: unique keys, finite ranges, default inside.
    let mut keys = BTreeSet::new();
    for s in &kit.settings {
        if s.key.trim().is_empty() || !keys.insert(s.key.as_str()) {
            issues.push(bad(format!("kit `{}` setting key `{}` is empty or repeated", kit.id, s.key)));
        }
        if !(s.min.is_finite() && s.max.is_finite() && s.default.is_finite() && s.min <= s.max && s.default >= s.min && s.default <= s.max) {
            issues.push(bad(format!("kit `{}` setting `{}` has a bad range or default", kit.id, s.key)));
        }
    }

    // Graph fragment: well formed, part references inside the kit, no scripts.
    let mut graph_issues = Vec::new();
    check_graph(&kit.graph, &[], &format!("kit `{}` graph", kit.id), false, &mut graph_issues);
    issues.extend(graph_issues);
    for n in &kit.graph.nodes {
        if let Some(slot) = node_slot(&n.kind) {
            if slot.index() >= kit.parts.len() {
                issues.push(Issue::BadGraph { detail: format!("kit `{}` node {} names kit part {}, which does not exist", kit.id, n.id.0, slot.0) });
            }
        }
    }
    if let Some(o) = &kit.officer {
        if o.power_priority.values().any(|p| !(1..=5).contains(p)) || o.sliders.values().any(|v| !v.is_finite()) {
            issues.push(bad(format!("kit `{}` officer settings hold out-of-range values", kit.id)));
        }
    }

    if ruleset.kind.max_layer() < ControlLayer::L3Graph && needs_graph_layer(kit, catalogue) {
        issues.push(Issue::LayerNotAllowed);
    }
    issues
}
