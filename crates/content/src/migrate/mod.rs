//! Unit `content-migrate`. Explicit format migrations n -> n+1 for blueprints, kits and saves; never deleted.
//! A version 1 file loads in every later release. DESIGN.md L2083-2085.
use crate::cell::CellPos;
mod parts;
mod scan;
#[cfg(test)]
mod tests;

use crate::error::{ContentError, ContentResult};
use crate::ids::PartId;

/// A part a migration removed with no named replacement: its cells became empty and Ship's Notes lists it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RemovedPart { pub part: PartId, pub pos: CellPos }

/// One migration step over RON text. Steps work on the untyped RON value so old shapes need no Rust types.
pub struct Migration { pub from: u32, pub apply: fn(&str) -> ContentResult<String> }

/// Registered blueprint migrations in ascending `from` order. Empty while the format is 1.
pub static BLUEPRINT_MIGRATIONS: &[Migration] = &[];
pub static KIT_MIGRATIONS: &[Migration] = &[];
pub static SAVE_MIGRATIONS: &[Migration] = &[];

/// Read the top-level `format` integer without parsing the whole document (works on any format version).
/// Unit: `content-migrate`.
pub fn peek_format(ron_text: &str) -> ContentResult<u32> { scan::find_format(ron_text).map(|(_, v)| v) }

/// Rewrite the document's `format` integer (the one [`peek_format`] reads) to `format`, leaving every other byte
/// as it was. Migration steps end with this so the runner sees the version advance.
pub fn set_format(ron_text: &str, format: u32) -> ContentResult<String> {
    let (span, _) = scan::find_format(ron_text)?;
    let mut out = String::with_capacity(ron_text.len() + 4);
    out.push_str(&ron_text[..span.start]);
    out.push_str(&format.to_string());
    out.push_str(&ron_text[span.end..]);
    Ok(out)
}

/// Apply every step from `from` up to `to`. Errors with `FormatTooNew` when `from > to`.
/// Unit: `content-migrate`.
pub fn migrate_text(ron_text: &str, steps: &[Migration], from: u32, to: u32) -> ContentResult<String> {
    if from > to {
        return Err(ContentError::FormatTooNew { found: from, supported: to, needs_release: release_for_format(from) });
    }
    let mut text = ron_text.to_owned();
    for n in from..to {
        let fail = |message: String| ContentError::Migration { from: n, to: n + 1, message };
        let step = steps.iter().find(|m| m.from == n).ok_or_else(|| fail("no migration is registered for this step".into()))?;
        text = (step.apply)(&text).map_err(|e| match e {
            e @ ContentError::Migration { .. } => e,
            other => fail(other.to_string()),
        })?;
        // A step that forgets to bump the version would make the next load run it again.
        match peek_format(&text) {
            Ok(v) if v == n + 1 => {}
            Ok(v) => return Err(fail(format!("step wrote format {v}"))),
            Err(e) => return Err(fail(format!("step output has no readable format: {e}"))),
        }
    }
    Ok(text)
}

/// Replace parts the catalogue marks `replaced_by`; parts with no replacement are removed, their cells emptied
/// and reported (L2083). Unit: `content-migrate`.
pub fn replace_removed_parts(bp: &mut crate::blueprint::Blueprint, catalogue: &crate::catalogue::Catalogue) -> Vec<RemovedPart> { parts::replace_removed_parts(bp, catalogue) }

/// The release that first reads a given blueprint format, for the "needs release" message. Unit: `content-migrate`.
pub fn release_for_format(format: u32) -> String {
    match FORMAT_RELEASES.iter().find(|(f, _)| *f == format) {
        Some((_, release)) => (*release).to_owned(),
        // A format this build has never heard of comes from a later release than this one.
        None => format!("later than {}", env!("CARGO_PKG_VERSION")),
    }
}

/// First release that reads each blueprint format. Every format bump appends a row with the bump's release.
pub const FORMAT_RELEASES: &[(u32, &str)] = &[(1, "0.1.0")];
