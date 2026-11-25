//! Test-only helpers shared by the text and officer-board tests: a CSV reader matching Godot's
//! `get_csv_line` (comma separated, double-quoted fields, doubled quotes) and the copy checks the CI lint
//! enforces on English rows (L1695-1706).
use crate::protocol::text::{Msg, MsgArg};
use std::collections::BTreeMap;

/// Parse a translation CSV into key -> (context, English). Panics on a malformed header or row.
pub(crate) fn parse(csv: &str) -> BTreeMap<String, (String, String)> {
    let mut lines = csv.lines();
    let header = split(lines.next().expect("header"));
    assert_eq!(header, vec!["keys", "?context", "en"], "CSV header");
    let mut rows = BTreeMap::new();
    for line in lines {
        if line.trim().is_empty() { continue; }
        let f = split(line);
        assert_eq!(f.len(), 3, "row needs three fields: {line}");
        assert!(!f[1].is_empty() && !f[2].is_empty(), "empty context or text: {line}");
        assert!(rows.insert(f[0].clone(), (f[1].clone(), f[2].clone())).is_none(), "duplicate key {}", f[0]);
    }
    rows
}

fn split(line: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut cur = String::new();
    let mut quoted = false;
    let mut chars = line.chars().peekable();
    while let Some(c) = chars.next() {
        match c {
            '"' if quoted && chars.peek() == Some(&'"') => { cur.push('"'); chars.next(); }
            '"' => quoted = !quoted,
            ',' if !quoted => out.push(std::mem::take(&mut cur)),
            _ => cur.push(c),
        }
    }
    assert!(!quoted, "unterminated quote: {line}");
    out.push(cur);
    out
}

/// Highest placeholder index + 1 used by an English string ({0}, {1}...), and that every lower index is used.
pub(crate) fn placeholder_count(en: &str) -> usize {
    let mut used = BTreeMap::new();
    let b = en.as_bytes();
    let mut i = 0;
    while i < b.len() {
        if b[i] == b'{' {
            if let Some(end) = en[i..].find('}') {
                if let Ok(n) = en[i + 1..i + end].parse::<usize>() { used.insert(n, ()); }
                i += end;
            }
        }
        i += 1;
    }
    let n = used.keys().next_back().map(|k| k + 1).unwrap_or(0);
    assert_eq!(used.len(), n, "placeholders skip an index: {en}");
    n
}

const BANNED: &[&str] = &["introducing", "unlock", "empower", "elevate", "streamline", "transform", "discover", "seamless", "smart", "next-gen", "revolutionize", "leverage", "delve", "vibrant", "pivotal", "showcase", "tapestry", "testament", "unleash", "new"];

/// Copy rules on one English row: no banned words, no exclamation marks, no emoji, sentence case (the first
/// letter may be capital; later capitalised words must be glossary words or placeholders), and a length limit
/// measured with each placeholder counted as `arg_len` characters.
pub(crate) fn lint(key: &str, en: &str, max_len: usize, glossary: &[&str]) {
    let lower = en.to_lowercase();
    for w in lower.split(|c: char| !c.is_alphanumeric() && c != '-') {
        assert!(!BANNED.contains(&w), "{key}: banned word {w:?} in {en:?}");
    }
    assert!(!en.contains('!'), "{key}: exclamation mark");
    assert!(en.chars().all(|c| (c as u32) < 0x2000), "{key}: emoji or pictograph");
    for (i, w) in en.split(' ').enumerate() {
        let w = w.trim_matches(|c: char| !c.is_alphanumeric() && c != '-');
        if i == 0 || w.is_empty() || w.starts_with('{') { continue; }
        if w.chars().next().map(|c| c.is_uppercase()).unwrap_or(false) {
            assert!(glossary.contains(&w), "{key}: capitalised word {w:?} is not a glossary word in {en:?}");
        }
    }
    let rendered = rendered_len(en);
    assert!(rendered <= max_len, "{key}: {rendered} characters rendered, limit {max_len}: {en:?}");
}

/// Length with each placeholder replaced by a typical 6-character value ("12.4 km", "T0042").
pub(crate) fn rendered_len(en: &str) -> usize {
    let mut n = 0;
    let mut in_ph = false;
    for c in en.chars() {
        match c { '{' => { in_ph = true; n += 6; } '}' => in_ph = false, _ if in_ph => {}, _ => n += 1 }
    }
    n
}

/// Assert a message's key has a row and the row's placeholders match the argument count.
pub(crate) fn check_msg(rows: &BTreeMap<String, (String, String)>, msg: &Msg) {
    let (_, en) = rows.get(&msg.key.0).unwrap_or_else(|| panic!("no CSV row for {}", msg.key.0));
    assert_eq!(placeholder_count(en), msg.args.len(), "{}: placeholder count vs args {:?}", msg.key.0, msg.args);
    for a in &msg.args {
        if let MsgArg::Num { value, .. } = a { assert!(value.is_finite(), "{}: non-finite arg", msg.key.0); }
    }
}

/// Capitalised words copy rule 1 allows inside a sentence: glossary codes, stance, rung, grade and billet words.
pub(crate) const GLOSSARY: &[&str] = &["ESM", "EMCON", "ID", "Friend", "Neutral", "Suspect", "Hostile", "Unknown", "Domain", "Type", "Class", "Exact", "Bearing", "Coarse", "Fine", "Lock", "Helm", "Engineer", "Sensors", "Gunnery", "Damage", "Control", "Combine", "PvP", "L0", "L1", "L2", "L3", "L4", "Salvage", "Intel", "RHIB"];
