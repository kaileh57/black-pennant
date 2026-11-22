//! Untrusted RON text checks shared by kit, blueprint and share-string loading: the size and nesting limits
//! checked before parsing (L2081), the ship-or-kit discriminator, and the stored `hash` field.
//!
//! The tokenizer understands line and nested block comments, strings with escapes, raw strings, byte strings
//! and char literals, so brackets or field names inside any of those never count.
use crate::blueprint::{MAX_RON_BYTES, MAX_RON_DEPTH};
use crate::error::{ContentError, ContentResult};
use crate::ids::ContentHash;
use std::ops::Range;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Tok {
    Open,
    Close,
    Colon,
    Ident,
    /// A string literal; the range covers the quotes.
    Str,
    Other,
}

struct Lexer<'a> {
    b: &'a [u8],
    i: usize,
}

impl<'a> Lexer<'a> {
    fn new(text: &'a str) -> Self { Self { b: text.as_bytes(), i: 0 } }
    fn at(&self, k: usize) -> Option<u8> { self.b.get(self.i + k).copied() }

    fn trivia(&mut self) {
        loop {
            match (self.at(0), self.at(1)) {
                (Some(c), _) if c.is_ascii_whitespace() => self.i += 1,
                (Some(b'/'), Some(b'/')) => {
                    while let Some(c) = self.at(0) {
                        self.i += 1;
                        if c == b'\n' {
                            break;
                        }
                    }
                }
                (Some(b'/'), Some(b'*')) => {
                    self.i += 2;
                    let mut depth = 1u32;
                    while depth > 0 {
                        match (self.at(0), self.at(1)) {
                            (None, _) => return,
                            (Some(b'/'), Some(b'*')) => { depth += 1; self.i += 2; }
                            (Some(b'*'), Some(b'/')) => { depth -= 1; self.i += 2; }
                            _ => self.i += 1,
                        }
                    }
                }
                _ => return,
            }
        }
    }

    fn quoted(&mut self, q: u8) {
        self.i += 1;
        while let Some(c) = self.at(0) {
            if c == b'\\' {
                self.i += 2;
            } else {
                self.i += 1;
                if c == q {
                    return;
                }
            }
        }
        self.i = self.i.min(self.b.len());
    }

    fn raw_start(&self, k: usize) -> bool {
        let mut k = k;
        while self.at(k) == Some(b'#') {
            k += 1;
        }
        self.at(k) == Some(b'"')
    }

    /// Raw string starting at `r`.
    fn raw(&mut self) {
        self.i += 1;
        let mut hashes = 0;
        while self.at(0) == Some(b'#') {
            hashes += 1;
            self.i += 1;
        }
        self.i += 1;
        while let Some(c) = self.at(0) {
            if c == b'"' && (1..=hashes).all(|k| self.at(k) == Some(b'#')) {
                self.i += 1 + hashes;
                return;
            }
            self.i += 1;
        }
    }

    fn next(&mut self) -> Option<(Tok, Range<usize>)> {
        self.trivia();
        let start = self.i;
        let c = self.at(0)?;
        let tok = match c {
            b'(' | b'[' | b'{' => { self.i += 1; Tok::Open }
            b')' | b']' | b'}' => { self.i += 1; Tok::Close }
            b':' => { self.i += 1; Tok::Colon }
            b'"' => { self.quoted(b'"'); Tok::Str }
            b'\'' => { self.quoted(b'\''); Tok::Other }
            b'b' if self.at(1) == Some(b'"') => { self.i += 1; self.quoted(b'"'); Tok::Other }
            b'b' if self.at(1) == Some(b'r') && self.raw_start(2) => { self.i += 1; self.raw(); Tok::Other }
            b'r' if self.raw_start(1) => { self.raw(); Tok::Other }
            c if c.is_ascii_alphabetic() || c == b'_' => {
                if c == b'r' && self.at(1) == Some(b'#') {
                    self.i += 2;
                }
                while matches!(self.at(0), Some(c) if c.is_ascii_alphanumeric() || c == b'_') {
                    self.i += 1;
                }
                Tok::Ident
            }
            b'#' if self.at(1) == Some(b'!') => {
                // `#![enable(...)]` attribute: its brackets are not document nesting.
                self.i += 2;
                let mut d = 0i32;
                loop {
                    self.trivia();
                    match self.at(0) {
                        None => break,
                        Some(b'[') | Some(b'(') => { d += 1; self.i += 1; }
                        Some(b']') | Some(b')') => {
                            d -= 1;
                            self.i += 1;
                            if d <= 0 {
                                break;
                            }
                        }
                        Some(b'"') => self.quoted(b'"'),
                        Some(_) => self.i += 1,
                    }
                }
                Tok::Other
            }
            _ => {
                // Multi-byte UTF-8 or punctuation: advance one byte (never splits a token we care about).
                self.i += 1;
                Tok::Other
            }
        };
        Some((tok, start..self.i))
    }
}

/// Deepest bracket nesting of the document.
pub(crate) fn max_depth(text: &str) -> usize {
    let mut lx = Lexer::new(text);
    let (mut depth, mut max) = (0usize, 0usize);
    while let Some((tok, _)) = lx.next() {
        match tok {
            Tok::Open => {
                depth += 1;
                max = max.max(depth);
            }
            Tok::Close => depth = depth.saturating_sub(1),
            _ => {}
        }
    }
    max
}

/// Refuse untrusted RON over 16 MiB or nested deeper than 128 before any parsing (L2081).
pub(crate) fn check_untrusted(text: &str) -> ContentResult<()> {
    if text.len() > MAX_RON_BYTES {
        return Err(ContentError::Limit("RON text over 16 MiB"));
    }
    if max_depth(text) > MAX_RON_DEPTH {
        return Err(ContentError::Limit("RON nesting deeper than 128"));
    }
    Ok(())
}

/// What a top-level document is, by its fields: a blueprint has a `header`, a kit has `format` at top level.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum DocKind { Ship, Kit }

/// Walk the top-level struct's field names (depth 1) and, inside a top-level `header`, the header's (depth 2).
/// `f` gets (inside header, name, lexer positioned after the colon); returning `Some` stops the walk.
fn walk_fields<T>(text: &str, mut f: impl FnMut(bool, &str, &mut Lexer<'_>) -> Option<T>) -> Option<T> {
    let mut lx = Lexer::new(text);
    let mut depth = 0usize;
    let mut header_open = false;
    let mut in_header = false;
    let mut pending: Option<Range<usize>> = None;
    while let Some((tok, span)) = lx.next() {
        match tok {
            Tok::Open => {
                depth += 1;
                if header_open && depth == 2 {
                    in_header = true;
                }
                header_open = false;
                pending = None;
            }
            Tok::Close => {
                if in_header && depth == 2 {
                    in_header = false;
                }
                depth = depth.saturating_sub(1);
                if depth == 0 {
                    return None;
                }
                pending = None;
            }
            Tok::Ident => pending = Some(span),
            Tok::Colon => {
                let Some(name) = pending.take() else { continue };
                let name = text[name].trim_start_matches("r#");
                if depth == 1 && name == "header" {
                    header_open = true;
                    continue;
                }
                if depth == 1 || (in_header && depth == 2) {
                    if let Some(v) = f(in_header, name, &mut lx) {
                        return Some(v);
                    }
                }
            }
            _ => pending = None,
        }
    }
    None
}

pub(crate) fn doc_kind(text: &str) -> Option<DocKind> {
    walk_fields(text, |in_header, name, _| match (in_header, name) {
        (true, _) => Some(DocKind::Ship),
        (false, "format" | "id") => Some(DocKind::Kit),
        _ => None,
    })
}

/// The stored `hash` field: byte span of its string literal (quotes included) and its value. Blueprints keep it
/// in `header`, kits at top level; embedded kits' hashes are deeper and never match.
pub(crate) fn stored_hash(text: &str) -> Option<(Range<usize>, Option<ContentHash>)> {
    walk_fields(text, |_, name, lx| {
        if name != "hash" {
            return None;
        }
        match lx.next() {
            Some((Tok::Str, span)) => {
                // An unterminated literal runs to the end of the text: no value (and no slicing inside a char).
                let closed = span.len() >= 2 && text.as_bytes()[span.end - 1] == b'"';
                let inner = if closed { text.get(span.start + 1..span.end - 1) } else { None };
                Some((span, inner.and_then(ContentHash::from_hex)))
            }
            _ => Some((0..0, None)),
        }
    })
    .filter(|(span, _)| !span.is_empty())
}

/// Check that text written by the game's serializer still matches the hash it carries: BLAKE3 of the same text
/// with the hash literal emptied must equal the stored value (L2079). Works for any format version, because it
/// needs no re-serialisation. Returns the hash on success; `None` when it is missing, empty or wrong.
pub(crate) fn verify_text_hash(text: &str) -> Option<ContentHash> {
    let (span, stored) = stored_hash(text)?;
    let stored = stored.filter(|h| !h.is_empty())?;
    let mut hasher = blake3::Hasher::new();
    hasher.update(text[..span.start].as_bytes());
    hasher.update(b"\"\"");
    hasher.update(text[span.end..].as_bytes());
    let got = ContentHash(*hasher.finalize().as_bytes());
    (got == stored).then_some(stored)
}

/// Point a header-scan parse error at the file being loaded.
pub(crate) fn rename_file(e: ContentError, file: &str) -> ContentError {
    match e {
        ContentError::Parse { message, .. } => ContentError::Parse { file: file.to_string(), message },
        other => other,
    }
}

/// The game serializer's canonical RON (L2079): fields in declaration order, LF line endings, 4-space indent,
/// no comments, no struct names, no extensions (options are written `Some(..)`), escaped strings. Nesting
/// beyond two levels is written on one line, so each cell row, part, kit placement and rule is one line and a
/// hull edit changes only the lines it touches in a diff. Every option is set explicitly so a ron default
/// change cannot move the bytes, and so Windows writes the same LF text as macOS.
pub(crate) fn to_canonical<T: serde::Serialize>(value: &T) -> String {
    let cfg = ron::ser::PrettyConfig::new()
        .depth_limit(2)
        .new_line("\n")
        .indentor("    ")
        .separator(" ")
        .struct_names(false)
        .separate_tuple_members(false)
        .enumerate_arrays(false)
        .compact_arrays(false)
        .extensions(ron::extensions::Extensions::empty())
        .escape_strings(true)
        .compact_structs(false)
        .compact_maps(false)
        .number_suffixes(false);
    let mut out = ron::Options::default()
        .without_recursion_limit()
        .to_string_pretty(value, cfg)
        // Writing to a String cannot fail and every content type serialises; a failure is a programming error.
        .expect("content types always serialise to RON");
    out.push('\n');
    out
}
