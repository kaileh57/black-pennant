//! Contract addition made by unit `ui-blueprints` (additive; no other unit's body is touched).
//! DESIGN.md L2093: "Copy is a button in the editor and the blueprint browser." `share_string` covers only
//! the design under edit, so the browser had no way to copy a file it lists without loading that file into
//! the dock (which would replace the player's open design). This gives the browser a share string for any
//! blueprint or kit file it shows, built the same way: `content::share::encode_share_string` over the
//! canonicalised item, so the string a file gives equals the one the dock gives for the same design.
//! Every path is untrusted input: a missing file, a file over the 16 MiB RON limit or one that does not
//! parse gives `""`, never a panic.
use super::BpEditor;
use content::blueprint::Shareable;
use godot::classes::FileAccess;
use godot::prelude::*;

#[godot_api(secondary)]
impl BpEditor {
    /// A blueprint (`.bp.ron`) or kit (`.kit.ron`) file as a share string (`BPN1:` then base64url of the
    /// compressed canonical RON), for the blueprint browser's Copy. `path` is a `user://` or `res://` path or
    /// a stock path as `Editor.browse` and `Content.starters` give them (`stock:starters/...`,
    /// `starters/...`). Returns `""` when the file is missing, too large or does not load.
    #[func]
    fn file_share_string(&self, path: GString) -> GString { GString::from(share_string_of_file(&path.to_string()).unwrap_or_default().as_str()) }
}

/// The share string of a file, or `None` when it does not load.
pub(crate) fn share_string_of_file(path: &str) -> Option<String> {
    let path = path.trim();
    if path.is_empty() { return None; }
    let item = if path.starts_with("user://") || path.starts_with("res://") {
        if !FileAccess::file_exists(path) { return None; }
        let len = FileAccess::open(path, godot::classes::file_access::ModeFlags::READ).map_or(0, |f| f.get_length());
        if len == 0 || len > content::blueprint::MAX_RON_BYTES as u64 { return None; }
        shareable_from_text(&FileAccess::get_file_as_string(path).to_string(), path)?
    } else {
        Shareable::Ship(Box::new(content::campaign::load::stock_blueprint(&stock_key(path)).ok()?))
    };
    Some(content::share::encode_share_string(&item))
}

/// A stock path as `content::campaign::load::stock_blueprint` takes it: no `stock:` scheme, forward slashes.
pub(crate) fn stock_key(path: &str) -> String {
    let p = path.trim().replace('\\', "/");
    let p = p.strip_prefix("stock:").unwrap_or(&p).trim_start_matches('/');
    p.to_string()
}

/// Parse file text as a ship or a kit by its file name (`.kit.ron` is a kit, anything else a ship).
pub(crate) fn shareable_from_text(text: &str, file: &str) -> Option<Shareable> {
    if text.len() > content::blueprint::MAX_RON_BYTES { return None; }
    if file.to_ascii_lowercase().ends_with(".kit.ron") {
        let mut kit = content::kit::parse_kit(text, file).ok()?;
        content::kit::canonicalize_kit(&mut kit);
        Some(Shareable::Kit(Box::new(kit)))
    } else {
        let mut bp = content::blueprint::io::load_blueprint(text, file).ok()?.blueprint;
        content::blueprint::io::canonicalize_blueprint(&mut bp);
        bp.header.hash = content::blueprint::io::compute_hash(&bp);
        Some(Shareable::Ship(Box::new(bp)))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn stock_keys_lose_the_scheme_and_backslashes() {
        assert_eq!(stock_key("stock:starters/fac_22.bp.ron"), "starters/fac_22.bp.ron");
        assert_eq!(stock_key(" starters\\fac_22.bp.ron "), "starters/fac_22.bp.ron");
        assert_eq!(stock_key("stock:/classes/a.bp.ron"), "classes/a.bp.ron");
    }

    #[test]
    fn bad_text_gives_nothing() {
        assert!(shareable_from_text("not ron at all (", "x.bp.ron").is_none());
        assert!(shareable_from_text("(", "x.kit.ron").is_none());
        assert!(shareable_from_text("", "x.bp.ron").is_none());
    }

    #[test]
    fn a_ship_file_round_trips_through_its_share_string() {
        let bp = content::blueprint::io::new_blueprint("Round trip", 24.0);
        let text = content::blueprint::io::to_file_text(&bp);
        let item = shareable_from_text(&text, "round_trip.bp.ron").expect("a new design loads");
        let s = content::share::encode_share_string(&item);
        assert!(s.starts_with(content::share::SHARE_PREFIX));
        let back = content::share::decode_share_string(&s).expect("its share string decodes");
        match (item, back) {
            (Shareable::Ship(a), Shareable::Ship(b)) => {
                assert_eq!(a.header.name, b.header.name);
                assert_eq!(content::blueprint::io::compute_hash(&a), content::blueprint::io::compute_hash(&b));
            }
            _ => panic!("a ship stays a ship"),
        }
    }
}
