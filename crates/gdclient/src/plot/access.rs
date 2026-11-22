//! Part of unit `gd-plot`. The plot's screen-reader tree (L1743, accessibility floor 12): like ItemList, the
//! plot is a `ROLE_LIST_BOX` with one `ROLE_LIST_BOX_OPTION` sub-element per track, named as the track table
//! names it ("Track 0412, Hostile, bearing 047"), with bounds, selection and the click and focus actions.
//! Rebuilt on `NOTIFICATION_ACCESSIBILITY_UPDATE`; the plot queues that update when the track set or the
//! selection changes and at 1 Hz for positions.
//!
//! Actions arrive through callables that only queue a request; the plot drains the queue in `process`, so an
//! action never re-enters the plot while it is borrowed.
use super::picture::{Picture, SymKind};
use godot::classes::accessibility_server::{AccessibilityAction, AccessibilityRole};
use godot::classes::AccessibilityServer;
use godot::obj::Singleton;
use godot::prelude::*;
use sim_systems::ids::TrackId;
use std::cell::RefCell;
use std::collections::BTreeMap;
use std::rc::Rc;

pub const TEXT_KEYS: &[&str] = &["plot.access.name", "plot.access.description"];

/// What a screen reader asked of a track.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Request { Click(TrackId), Focus(TrackId) }

#[derive(Default)]
pub struct Access {
    /// Sub-element per track, ascending track number.
    elems: BTreeMap<TrackId, Rid>,
    queue: Rc<RefCell<Vec<Request>>>,
    /// The track the screen reader focused last (when nothing is selected).
    pub focus: TrackId,
    /// Track set at the last update, for change detection.
    pub ids: Vec<TrackId>,
}

impl Access {
    /// Sub-elements alive now.
    pub fn count(&self) -> usize { self.elems.len() }
    pub fn take_requests(&mut self) -> Vec<Request> { std::mem::take(&mut *self.queue.borrow_mut()) }
    /// The engine dropped every element (`NOTIFICATION_ACCESSIBILITY_INVALIDATE`): the sub-elements went with
    /// the parent, so forget them without freeing.
    pub fn invalidate(&mut self) { self.elems.clear(); }
    /// The element to report as focused: the selection's, else the focused track's.
    pub fn focused(&self, selected: TrackId) -> Option<Rid> { self.elems.get(&selected).or_else(|| self.elems.get(&self.focus)).copied() }

    /// Rebuild the tree under `parent` from the last picture.
    pub fn update(&mut self, parent: Rid, pic: &Picture, selected: TrackId, name: &str, description: &str) {
        if !parent.is_valid() { return; }
        let mut srv = AccessibilityServer::singleton();
        srv.update_set_role(parent, AccessibilityRole::LIST_BOX);
        srv.update_set_name(parent, name);
        srv.update_set_description(parent, description);
        let tracks: Vec<_> = pic.symbols.iter().filter(|s| s.kind == SymKind::Track && !s.track.is_none()).collect();
        srv.update_set_list_item_count(parent, tracks.len() as i32);
        self.ids = tracks.iter().map(|s| s.track).collect();
        // Free the elements of tracks that left the picture.
        let gone: Vec<TrackId> = self.elems.keys().filter(|t| !self.ids.contains(t)).copied().collect();
        for t in gone {
            if let Some(rid) = self.elems.remove(&t) { if srv.has_element(rid) { srv.free_element(rid); } }
        }
        let half = super::core::symbol_half(1.0).max(12.0);
        for (index, s) in tracks.iter().enumerate() {
            let rid = match self.elems.get(&s.track).copied().filter(|r| srv.has_element(*r)) {
                Some(r) => r,
                None => {
                    let r = srv.create_sub_element(parent, AccessibilityRole::LIST_BOX_OPTION);
                    if !r.is_valid() { continue; }
                    self.elems.insert(s.track, r);
                    r
                }
            };
            srv.update_set_name(rid, s.name.as_str());
            srv.update_set_list_item_index(rid, index as i32);
            srv.update_set_list_item_level(rid, 0);
            srv.update_set_list_item_selected(rid, s.track == selected);
            srv.update_set_bounds(rid, Rect2::new(Vector2::new(s.pos.x - half, s.pos.y - half), Vector2::new(2.0 * half, 2.0 * half)));
            for (action, req) in [(AccessibilityAction::CLICK, Request::Click(s.track)), (AccessibilityAction::FOCUS, Request::Focus(s.track))] {
                let queue = self.queue.clone();
                let callable = Callable::from_fn("plot_track_action", move |_args| { queue.borrow_mut().push(req); Variant::nil() });
                srv.update_add_action(rid, action, &callable);
            }
        }
    }
}
