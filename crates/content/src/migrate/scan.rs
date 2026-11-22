//! A small RON token scanner that finds the `format` field without parsing the document. It understands
//! comments (line and nested block), strings, raw strings, byte strings, char literals and `#![...]` attributes,
//! so a `format:` inside any of those is never mistaken for the field.
use crate::error::{ContentError, ContentResult};

fn err(message: impl Into<String>) -> ContentError {
    ContentError::Parse { file: "format header".into(), message: message.into() }
}

struct Scanner<'a> {
    b: &'a [u8],
    i: usize,
}

impl<'a> Scanner<'a> {
    fn peek(&self) -> Option<u8> { self.b.get(self.i).copied() }
    fn at(&self, k: usize) -> Option<u8> { self.b.get(self.i + k).copied() }

    /// Skip whitespace and comments.
    fn trivia(&mut self) -> ContentResult<()> {
        loop {
            match (self.peek(), self.at(1)) {
                (Some(c), _) if c.is_ascii_whitespace() => self.i += 1,
                (Some(b'/'), Some(b'/')) => {
                    while let Some(c) = self.peek() {
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
                        match (self.peek(), self.at(1)) {
                            (None, _) => return Err(err("unterminated block comment")),
                            (Some(b'/'), Some(b'*')) => { depth += 1; self.i += 2; }
                            (Some(b'*'), Some(b'/')) => { depth -= 1; self.i += 2; }
                            _ => self.i += 1,
                        }
                    }
                }
                _ => return Ok(()),
            }
        }
    }

    /// Skip a quoted string or char literal starting at the quote.
    fn quoted(&mut self, q: u8) -> ContentResult<()> {
        self.i += 1;
        loop {
            match self.peek() {
                None => return Err(err("unterminated string")),
                Some(b'\\') => self.i += 2,
                Some(c) if c == q => { self.i += 1; return Ok(()); }
                Some(_) => self.i += 1,
            }
        }
    }

    /// Skip a raw string starting at `r`: `r"..."`, `r#"..."#` and so on.
    fn raw(&mut self) -> ContentResult<()> {
        self.i += 1;
        let mut hashes = 0;
        while self.peek() == Some(b'#') {
            hashes += 1;
            self.i += 1;
        }
        if self.peek() != Some(b'"') {
            return Err(err("malformed raw string"));
        }
        self.i += 1;
        loop {
            match self.peek() {
                None => return Err(err("unterminated raw string")),
                Some(b'"') if (1..=hashes).all(|k| self.at(k) == Some(b'#')) => {
                    self.i += 1 + hashes;
                    return Ok(());
                }
                Some(_) => self.i += 1,
            }
        }
    }

    /// True when the text at offset `k` is `#`* then `"`: the rest of a raw string opener after its `r`.
    fn raw_start(&self, k: usize) -> bool {
        let mut k = k;
        while self.at(k) == Some(b'#') {
            k += 1;
        }
        self.at(k) == Some(b'"')
    }

    fn ident(&mut self) -> &'a [u8] {
        let start = self.i;
        while matches!(self.peek(), Some(c) if c.is_ascii_alphanumeric() || c == b'_') {
            self.i += 1;
        }
        &self.b[start..self.i]
    }
}

/// Byte span of the integer literal of the document's `format` field, and its value.
///
/// The field is `format` directly inside the top-level struct (kits), or `format` inside the top-level field
/// `header` (blueprints and saves). Fields deeper than that, such as an embedded kit's own `format`, are ignored.
pub fn find_format(text: &str) -> ContentResult<(std::ops::Range<usize>, u32)> {
    let mut s = Scanner { b: text.as_bytes(), i: 0 };
    let mut depth = 0u32;
    // True while inside the top-level `header` field's value.
    let mut in_header = false;
    let mut header_depth = 0u32;
    loop {
        s.trivia()?;
        let Some(c) = s.peek() else { return Err(err("no format field")) };
        match c {
            b'#' if depth == 0 && s.at(1) == Some(b'!') => {
                // Extension attribute `#![enable(...)]`: skip to its closing bracket.
                s.i += 2;
                let mut d = 0i32;
                loop {
                    s.trivia()?;
                    match s.peek() {
                        None => return Err(err("unterminated attribute")),
                        Some(b'[') | Some(b'(') => { d += 1; s.i += 1; }
                        Some(b']') | Some(b')') => {
                            d -= 1;
                            s.i += 1;
                            if d <= 0 { break; }
                        }
                        Some(b'"') => s.quoted(b'"')?,
                        Some(_) => s.i += 1,
                    }
                }
            }
            b'(' | b'[' | b'{' => { depth += 1; s.i += 1; }
            b')' | b']' | b'}' => {
                if depth == 0 {
                    return Err(err("unbalanced brackets"));
                }
                depth -= 1;
                s.i += 1;
                if in_header && depth < header_depth {
                    in_header = false;
                }
                if depth == 0 {
                    return Err(err("no format field"));
                }
            }
            b'"' => s.quoted(b'"')?,
            b'\'' => s.quoted(b'\'')?,
            b'b' if s.at(1) == Some(b'"') => { s.i += 1; s.quoted(b'"')?; }
            b'b' if s.at(1) == Some(b'r') && s.raw_start(2) => { s.i += 1; s.raw()?; }
            b'r' if s.raw_start(1) => s.raw()?,
            c if c.is_ascii_alphabetic() || c == b'_' => {
                // `r#name` raw identifiers.
                if c == b'r' && s.at(1) == Some(b'#') {
                    s.i += 2;
                }
                let name = s.ident();
                s.trivia()?;
                if s.peek() != Some(b':') {
                    continue;
                }
                s.i += 1;
                if depth == 1 && name == b"header" {
                    in_header = true;
                    header_depth = 2;
                    continue;
                }
                let wanted = name == b"format" && (depth == 1 || (in_header && depth == header_depth));
                if !wanted {
                    continue;
                }
                s.trivia()?;
                if s.peek() == Some(b'+') {
                    s.i += 1;
                }
                let start = s.i;
                while matches!(s.peek(), Some(c) if c.is_ascii_alphanumeric() || c == b'_') {
                    s.i += 1;
                }
                let value = parse_u32(&text[start..s.i]).ok_or_else(|| err(format!("format is not a version number: `{}`", &text[start..s.i])))?;
                return Ok((start..s.i, value));
            }
            _ => s.i += 1,
        }
    }
}

fn parse_u32(lit: &str) -> Option<u32> {
    let clean: String = lit.chars().filter(|c| *c != '_').collect();
    let (digits, radix) = match clean.get(..2) {
        Some("0x") => (&clean[2..], 16),
        Some("0o") => (&clean[2..], 8),
        Some("0b") => (&clean[2..], 2),
        _ => (&clean[..], 10),
    };
    if digits.is_empty() {
        return None;
    }
    u32::from_str_radix(digits, radix).ok()
}
