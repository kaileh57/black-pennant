//! Unit `xt-lint-copy`. Snippet lints over player-facing text. Owns `game/ui/lint/banned_phrases.txt` (the
//! CLAUDE.md banned words: introducing, unlock, empower, elevate, streamline, transform, discover, seamless,
//! smart, next-gen, revolutionize, leverage, delve, vibrant, pivotal, showcase, tapestry, testament, unleash;
//! plus the copy-rule-9 tics of L1703) and `game/ui/lint/glossary.txt` (codes allowed in capitals, L1707).
//! Scopes: `game/i18n/*.csv` (the English column), `README.md`, `docs/**/*.md`, release notes; never Rust or
//! xtask sources. Radio chatter, save rows and player-typed text are exempt (L1608).
//!
//! Also owns `game/ui/lint/proper_nouns.txt`: names that keep their capitals (the game's title, platforms and
//! third-party screen names quoted in the README). A proper noun is masked before the banned-word and case
//! checks, so "Smart App Control" is not the banned "smart". Stance, rung, grade and billet words (L1695) come
//! from the CSV rows whose context names them ("Glossary stance word", "Officer billet", ...).
//!
//! Sources of copy: every row of `game/i18n/*.csv` (English column, context column for exemptions), the prose of
//! the web documents (Markdown outside code blocks and inline code), and literal (non-key) `text`,
//! `tooltip_text`, `placeholder_text` and `title` values in `.tscn` scenes.
use super::godot::{exempt, find_word_ci, finding, parse_resource, scene_nodes, strip_gd_comments, unquote};
use super::{Finding, LintCtx, Severity};

const BANNED: &str = "game/ui/lint/banned_phrases.txt";
const GLOSSARY: &str = "game/ui/lint/glossary.txt";
const PROPER: &str = "game/ui/lint/proper_nouns.txt";

// ------------------------------------------------------------------------------------------------
// CSV and copy collection
// ------------------------------------------------------------------------------------------------

/// RFC 4180 rows as (line of the row's first character, cells). Quoted fields may hold commas, doubled quotes
/// and newlines; `\r` before a newline is dropped.
pub(super) fn parse_csv(text: &str) -> Vec<(u32, Vec<String>)> {
    let mut rows = Vec::new();
    let mut row: Vec<String> = Vec::new();
    let mut cell = String::new();
    let mut in_q = false;
    let mut line = 1u32;
    let mut row_line = 1u32;
    let mut chars = text.chars().peekable();
    let mut any = false;
    while let Some(c) = chars.next() {
        if in_q {
            match c {
                '"' if chars.peek() == Some(&'"') => { cell.push('"'); chars.next(); }
                '"' => in_q = false,
                '\r' if chars.peek() == Some(&'\n') => {}
                '\n' => { line += 1; cell.push('\n'); }
                _ => cell.push(c),
            }
            continue;
        }
        match c {
            '"' if cell.is_empty() => { in_q = true; any = true; }
            ',' => { row.push(std::mem::take(&mut cell)); any = true; }
            '\r' if chars.peek() == Some(&'\n') => {}
            '\n' => {
                if any || !cell.is_empty() { row.push(std::mem::take(&mut cell)); rows.push((row_line, std::mem::take(&mut row))); }
                any = false;
                line += 1;
                row_line = line;
            }
            _ => { cell.push(c); any = true; }
        }
    }
    if any || !cell.is_empty() { row.push(cell); rows.push((row_line, row)); }
    rows
}

/// One translation CSV with its column roles.
pub(super) struct I18nFile { pub path: String, pub key_col: Option<usize>, pub ctx_col: Option<usize>, pub en_col: Option<usize>, pub header: Vec<String>, pub rows: Vec<(u32, Vec<String>)> }

impl I18nFile {
    pub fn parse(path: &str, text: &str) -> Self {
        let mut rows = parse_csv(text.strip_prefix('\u{feff}').unwrap_or(text));
        let header = if rows.is_empty() { Vec::new() } else { rows.remove(0).1 };
        let find = |p: &dyn Fn(&str) -> bool| header.iter().position(|h| p(h.trim()));
        I18nFile {
            path: path.to_string(),
            key_col: find(&|h| h == "keys"),
            ctx_col: find(&|h| h.starts_with("?context")),
            en_col: find(&|h| h == "en"),
            header: header.clone(),
            rows,
        }
    }
    pub fn cell<'a>(&self, row: &'a [String], col: Option<usize>) -> &'a str { col.and_then(|c| row.get(c)).map_or("", |s| s.as_str()) }
}

pub(super) fn i18n_files(ctx: &LintCtx) -> Vec<I18nFile> {
    ctx.files("game/i18n/", &["csv"]).map(|f| I18nFile::parse(f, &ctx.read(f))).collect()
}

/// Every key's English text, sorted by key (first CSV row wins), for resolving scene `text` values.
pub(super) fn english_map(ctx: &LintCtx) -> Vec<(String, String)> {
    let mut out: Vec<(String, String)> = Vec::new();
    for f in i18n_files(ctx) {
        for (_, r) in &f.rows {
            let k = f.cell(r, f.key_col).trim();
            if !k.is_empty() { out.push((k.to_string(), f.cell(r, f.en_col).to_string())); }
        }
    }
    out.sort_by(|a, b| a.0.cmp(&b.0));
    out.dedup_by(|a, b| a.0 == b.0);
    out
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Kind { Csv, Web, Scene }

/// One piece of copy.
#[derive(Clone, Debug)]
struct Snippet {
    path: String,
    line: u32,
    kind: Kind,
    key: String,
    context: String,
    text: String,
    /// A Markdown heading.
    heading: bool,
    /// A button label (CSV context mentions a button, or a Button node in a scene).
    button: bool,
    /// A Markdown line marked `<!-- ordered -->` (an ordered-content note).
    ordered_note: bool,
}

impl Snippet {
    /// Radio chatter, save rows and player-typed text (L1608).
    fn chatter_or_save(&self) -> bool {
        let c = self.context.to_ascii_lowercase();
        c.starts_with("radio") || c.contains("radio line") || c.contains("radio chatter") || c.contains("save row") || c.contains("save slot") || c.contains("player-typed") || c.contains("player typed")
    }
}

/// Whether a string is a translation key rather than text: lowercase dotted segments.
pub(super) fn is_key(s: &str) -> bool {
    let segs: Vec<&str> = s.split('.').collect();
    segs.len() >= 2
        && segs[0].starts_with(|c: char| c.is_ascii_lowercase())
        && segs.iter().all(|seg| !seg.is_empty() && seg.chars().all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '_'))
        && segs[1..].iter().any(|seg| seg.chars().any(|c| c.is_ascii_lowercase()))
}

fn csv_copy(ctx: &LintCtx) -> Vec<Snippet> {
    let mut out = Vec::new();
    for f in i18n_files(ctx) {
        for (ln, r) in &f.rows {
            let text = f.cell(r, f.en_col);
            if text.trim().is_empty() { continue; }
            let context = f.cell(r, f.ctx_col).to_string();
            let key = f.cell(r, f.key_col).to_string();
            let button = context.to_ascii_lowercase().contains("button") || key.split('.').any(|s| s == "button" || s == "btn");
            out.push(Snippet { path: f.path.clone(), line: *ln, kind: Kind::Csv, key, context, text: text.to_string(), heading: false, button, ordered_note: false });
        }
    }
    out
}

/// The web documents: README, docs, release notes and store copy.
fn web_files(ctx: &LintCtx) -> Vec<String> {
    ctx.files.iter().filter(|f| {
        let md = f.ends_with(".md");
        let top = matches!(f.as_str(), "README.md" | "CHANGELOG.md" | "RELEASES.md" | "RELEASE_NOTES.md");
        top || (md && (f.starts_with("docs/") || f.starts_with("release-notes/") || f.starts_with("releases/") || f.starts_with("store/"))) || (f.starts_with("store/") && f.ends_with(".txt"))
    }).cloned().collect()
}

/// Remove inline code spans, images, link targets, HTML tags and emphasis markers from one Markdown line.
fn md_inline(line: &str) -> String {
    let chars: Vec<char> = line.chars().collect();
    let mut out = String::with_capacity(line.len());
    let mut i = 0;
    while i < chars.len() {
        let c = chars[i];
        if c == '`' {
            let ticks = chars[i..].iter().take_while(|&&t| t == '`').count();
            let mut j = i + ticks;
            let mut closed = None;
            while j < chars.len() {
                if chars[j] == '`' {
                    let n = chars[j..].iter().take_while(|&&t| t == '`').count();
                    if n == ticks { closed = Some(j + n); break; }
                    j += n;
                } else { j += 1; }
            }
            match closed { Some(e) => { out.push(' '); i = e; } None => { i += ticks; } }
            continue;
        }
        if c == '!' && chars.get(i + 1) == Some(&'[') {
            // Image: drop alt text and target.
            if let Some(close) = chars[i..].iter().position(|&x| x == ')') { i += close + 1; continue; }
        }
        if c == ']' && chars.get(i + 1) == Some(&'(') {
            if let Some(close) = chars[i + 1..].iter().position(|&x| x == ')') { i += close + 2; continue; }
        }
        if c == '<' {
            if let Some(close) = chars[i..].iter().position(|&x| x == '>') {
                let tag: String = chars[i + 1..i + close].iter().collect();
                if tag.starts_with('/') || tag.starts_with('!') || tag.chars().next().map_or(false, |t| t.is_ascii_alphabetic()) {
                    // Autolinks `<https://...>` and tags vanish.
                    i += close + 1;
                    continue;
                }
            }
        }
        if matches!(c, '[' | '*') || (c == '_' && (i == 0 || !chars[i - 1].is_alphanumeric() || i + 1 >= chars.len() || !chars[i + 1].is_alphanumeric())) {
            i += 1;
            continue;
        }
        // Bare URLs are not prose.
        if (c == 'h' && chars[i..].starts_with(&['h', 't', 't', 'p'])) && (i == 0 || chars[i - 1].is_whitespace() || chars[i - 1] == '(') {
            let rest: String = chars[i..].iter().take(8).collect();
            if rest.starts_with("http://") || rest.starts_with("https://") {
                while i < chars.len() && !chars[i].is_whitespace() { i += 1; }
                continue;
            }
        }
        out.push(c);
        i += 1;
    }
    out
}

/// Prose lines of a Markdown document (code fences, HTML comments, tables' rule rows and front matter dropped).
fn md_copy(path: &str, text: &str) -> Vec<Snippet> {
    let mut out = Vec::new();
    let mut fence: Option<String> = None;
    let mut in_comment = false;
    for (i, raw) in text.lines().enumerate() {
        let ln = (i + 1) as u32;
        let t = raw.trim();
        if let Some(f) = &fence {
            if t.starts_with(f.as_str()) { fence = None; }
            continue;
        }
        if t.starts_with("```") || t.starts_with("~~~") { fence = Some(t[..3].to_string()); continue; }
        if in_comment {
            if t.contains("-->") { in_comment = false; }
            continue;
        }
        let ordered_note = t.contains("<!-- ordered");
        if t.starts_with("<!--") && !t.contains("-->") { in_comment = true; continue; }
        if raw.starts_with("    ") || raw.starts_with('\t') { continue; }
        if t.is_empty() || t.chars().all(|c| matches!(c, '|' | '-' | ':' | ' ' | '=' | '*' | '_')) {
            out.push(Snippet { path: path.into(), line: ln, kind: Kind::Web, key: String::new(), context: String::new(), text: String::new(), heading: false, button: false, ordered_note: false });
            continue;
        }
        let heading = t.starts_with('#');
        let mut body = t.trim_start_matches('#').trim_start();
        body = body.trim_start_matches('>').trim_start();
        // List markers.
        if let Some(rest) = body.strip_prefix("- ").or_else(|| body.strip_prefix("* ")).or_else(|| body.strip_prefix("+ ")) { body = rest; }
        let digits = body.chars().take_while(|c| c.is_ascii_digit()).count();
        if digits > 0 && body[digits..].starts_with(". ") { body = &body[digits + 2..]; }
        let body = md_inline(&body.replace('|', " "));
        out.push(Snippet { path: path.into(), line: ln, kind: Kind::Web, key: String::new(), context: String::new(), text: body.trim().to_string(), heading, button: false, ordered_note });
    }
    out
}

const SCENE_TEXT_PROPS: &[&str] = &["text", "tooltip_text", "placeholder_text", "title", "dialog_text"];

fn scene_copy(path: &str, text: &str) -> Vec<Snippet> {
    let mut out = Vec::new();
    for n in scene_nodes(&parse_resource(text)) {
        for p in SCENE_TEXT_PROPS {
            let Some(prop) = n.prop(p) else { continue };
            let v = prop.value.trim();
            if !v.starts_with('"') { continue; }
            let s = unquote(v);
            if s.trim().is_empty() || is_key(&s) { continue; }
            let button = n.ty.ends_with("Button") && *p == "text";
            out.push(Snippet { path: path.into(), line: prop.line, kind: Kind::Scene, key: format!("{}:{p}", n.path), context: String::new(), text: s, heading: false, button, ordered_note: false });
        }
    }
    out
}

fn all_copy(ctx: &LintCtx) -> Vec<Snippet> {
    let mut out = csv_copy(ctx);
    for f in web_files(ctx) { out.extend(md_copy(&f, &ctx.read(&f))); }
    for f in ctx.files("game/", &["tscn"]) {
        if exempt(f) { continue; }
        out.extend(scene_copy(f, &ctx.read(f)));
    }
    out.retain(|c| !c.text.is_empty() || c.kind == Kind::Web);
    out
}

/// Non-comment lines of a word list.
fn read_list(text: &str) -> Vec<String> {
    text.lines().map(str::trim).filter(|l| !l.is_empty() && !l.starts_with('#')).map(str::to_string).collect()
}

/// Proper nouns plus the stance, rung, grade and billet words from the CSVs, longest first.
fn capital_words(ctx: &LintCtx) -> Vec<String> {
    let mut out = read_list(&ctx.read(PROPER));
    for f in i18n_files(ctx) {
        for (_, r) in &f.rows {
            let c = f.cell(r, f.ctx_col).to_ascii_lowercase();
            if (c.starts_with("glossary") && c.ends_with("word")) || c.contains("billet") {
                let en = f.cell(r, f.en_col).trim();
                if !en.is_empty() { out.push(en.to_string()); }
            }
        }
    }
    out.sort_by(|a, b| b.len().cmp(&a.len()).then(a.cmp(b)));
    out.dedup();
    out
}

/// Replace case-sensitive whole-word occurrences of `nouns` with spaces (same byte length).
fn mask(text: &str, nouns: &[String]) -> String {
    let mut s = text.to_string();
    for n in nouns {
        let mut from = 0;
        while let Some(rel) = s[from..].find(n.as_str()) {
            let at = from + rel;
            let b = s[..at].chars().next_back();
            let a = s[at + n.len()..].chars().next();
            let edge = |c: Option<char>| c.map_or(true, |c| !c.is_alphanumeric());
            if edge(b) && edge(a) {
                let blank = " ".repeat(n.len());
                s.replace_range(at..at + n.len(), &blank);
            }
            from = at + n.len();
        }
    }
    s
}

// ------------------------------------------------------------------------------------------------
// banned_words
// ------------------------------------------------------------------------------------------------

/// Arrows that decorate buttons (directional arrows next to a direction word are allowed).
const ARROWS: &[&str] = &["→", "←", "➔", "➜", "➞", "⟶", "›", "»", "->", "=>", "⇒", "↗"];
const DIRECTIONS: &[&str] = &["port", "starboard", "left", "right", "up", "down", "north", "south", "east", "west", "ahead", "astern", "forward", "aft", "bow", "stern", "back", "next", "previous"];

/// Words and phrases from `banned_phrases.txt` (whole word, case-insensitive), button arrows, exclamation marks
/// on buttons. Unit: `xt-lint-copy`.
pub fn banned_words(ctx: &LintCtx) -> Vec<Finding> {
    let banned = read_list(&ctx.read(BANNED));
    let nouns = capital_words(ctx);
    let copy = all_copy(ctx);
    check_banned(&copy, &banned, &nouns)
}

/// Sentences of a text: split after `.`, `!`, `?` followed by whitespace (decimals and common abbreviations
/// kept together).
fn sentences(text: &str) -> Vec<String> {
    const ABBR: &[&str] = &["e.g.", "i.e.", "etc.", "vs.", "approx.", "no.", "mr.", "ms.", "dr.", "st."];
    let mut out = Vec::new();
    let mut cur = String::new();
    let chars: Vec<char> = text.chars().collect();
    for (i, &c) in chars.iter().enumerate() {
        cur.push(c);
        if matches!(c, '.' | '!' | '?') && chars.get(i + 1).map_or(true, |n| n.is_whitespace()) {
            let lower = cur.to_lowercase();
            let last = lower.split_whitespace().last().unwrap_or("");
            if c == '.' && ABBR.contains(&last) { continue; }
            if !cur.trim().is_empty() { out.push(cur.trim().to_string()); }
            cur.clear();
        }
    }
    if !cur.trim().is_empty() { out.push(cur.trim().to_string()); }
    out
}

fn word_count(s: &str) -> usize { s.split_whitespace().filter(|w| w.chars().any(|c| c.is_alphanumeric())).count() }

/// Snippet-rule-9 tics in one paragraph of copy.
fn tics(text: &str) -> Vec<String> {
    let mut out = Vec::new();
    let lower = text.to_lowercase();
    for opener in ["not just ", "not only ", "not merely ", "isn't just ", "is not just "] {
        if let Some(at) = lower.find(opener) {
            let tail = &lower[at + opener.len()..];
            let end = tail.find(['.', '!', '?']).unwrap_or(tail.len());
            let clause = &tail[..end];
            if clause.contains(", it") || clause.contains(" but ") || clause.contains("; it") || clause.contains(" — it") {
                out.push(format!("\"{}...\" construction (copy rule 9)", opener.trim()));
            }
        }
    }
    // "WORD — fragment": a capitalised word or code, then a dash, opening the text.
    let t = text.trim_start();
    let first: String = t.chars().take_while(|c| c.is_alphanumeric()).collect();
    if first.chars().filter(|c| c.is_alphabetic()).count() >= 2 && first.chars().all(|c| !c.is_lowercase()) {
        let rest = t[first.len()..].trim_start();
        if rest.starts_with('—') || rest.starts_with('–') || rest.starts_with("- ") || rest.starts_with("--") {
            out.push("\"WORD — fragment\" label (copy rule 9)".into());
        }
    }
    // Middle-dot taglines.
    if [" · ", " • ", " ∙ ", " ⋅ "].iter().any(|d| text.contains(d)) { out.push("middle-dot tagline (copy rule 9)".into()); }
    // Staccato headlines: three or more sentences of one or two words in a row.
    let mut run = 0;
    for s in sentences(text) {
        if word_count(&s) <= 2 && (s.ends_with('.') || s.ends_with('!')) { run += 1; } else { run = 0; }
        if run == 3 { out.push("staccato headline (copy rule 9)".into()); }
    }
    // Over one dash per paragraph.
    let dashes = text.matches('—').count() + text.matches(" – ").count() + text.matches(" -- ").count();
    if dashes > 1 { out.push(format!("{dashes} dashes in one paragraph; at most one (copy rule 9)")); }
    out
}

fn check_banned(copy: &[Snippet], banned: &[String], nouns: &[String]) -> Vec<Finding> {
    const L: &str = "copy.banned-words";
    let mut out = Vec::new();
    for c in copy.iter().filter(|c| !c.chatter_or_save()) {
        let masked = mask(&c.text, nouns);
        for b in banned {
            if !find_word_ci(&masked, b).is_empty() {
                out.push(finding(L, &c.path, c.line, format!("\"{b}\" is banned (game/ui/lint/banned_phrases.txt){}", key_note(c))));
            }
        }
        if c.button {
            let lower = c.text.to_lowercase();
            let directional = DIRECTIONS.iter().any(|d| !find_word_ci(&lower, d).is_empty());
            if !directional && ARROWS.iter().any(|a| c.text.contains(a)) {
                out.push(finding(L, &c.path, c.line, format!("arrow on a button; buttons are effect verbs (copy rule 3){}", key_note(c))));
            }
            if c.text.contains('!') { out.push(finding(L, &c.path, c.line, format!("exclamation mark on a button (copy rule 3){}", key_note(c)))); }
        }
        if c.kind != Kind::Web {
            for t in tics(&c.text) { out.push(finding(L, &c.path, c.line, format!("{t}{}", key_note(c)))); }
        }
    }
    // Web documents: tics per paragraph (consecutive prose lines).
    fn flush(para: &mut Vec<&Snippet>, out: &mut Vec<Finding>) {
        if let Some(first) = para.first() {
            let joined = para.iter().map(|c| c.text.as_str()).collect::<Vec<_>>().join(" ");
            for t in tics(&joined) { out.push(finding("copy.banned-words", &first.path, first.line, t)); }
        }
        para.clear();
    }
    let mut para: Vec<&Snippet> = Vec::new();
    for c in copy.iter().filter(|c| c.kind == Kind::Web) {
        let new_doc = para.first().map_or(false, |p| p.path != c.path);
        if c.text.is_empty() || c.heading || new_doc { flush(&mut para, &mut out); }
        if c.heading {
            for t in tics(&c.text) { out.push(finding(L, &c.path, c.line, t)); }
        } else if !c.text.is_empty() {
            para.push(c);
        }
    }
    flush(&mut para, &mut out);
    out
}

fn key_note(c: &Snippet) -> String { if c.key.is_empty() { String::new() } else { format!(" [{}]", c.key) } }

// ------------------------------------------------------------------------------------------------
// emoji
// ------------------------------------------------------------------------------------------------

/// Emoji and pictograph ranges (L1606), the same as `godot_patterns.txt` `emoji_range`.
const EMOJI_RANGES: &[(u32, u32)] = &[(0x1F000, 0x1FAFF), (0x2600, 0x27BF), (0x2300, 0x23FF), (0x2B00, 0x2BFF), (0x1F1E6, 0x1F1FF), (0xFE0F, 0xFE0F), (0x200D, 0x200D)];

fn first_emoji(s: &str) -> Option<char> { s.chars().find(|&c| EMOJI_RANGES.iter().any(|&(a, b)| (c as u32) >= a && (c as u32) <= b)) }

/// Emoji and pictographic code points in copy and in `.tscn` text. Unit: `xt-lint-copy`.
pub fn emoji(ctx: &LintCtx) -> Vec<Finding> {
    const L: &str = "copy.emoji";
    let mut out = Vec::new();
    let msg = |c: char| format!("emoji U+{:04X}; no emoji or Unicode pictographs as icons (L1606)", c as u32);
    for f in i18n_files(ctx) {
        for (ln, r) in &f.rows {
            for (i, cell) in r.iter().enumerate() {
                let h = f.header.get(i).map_or("", |h| h.trim());
                if Some(i) == f.key_col || h.starts_with('?') || h.starts_with('_') { continue; }
                if let Some(c) = first_emoji(cell) { out.push(finding(L, &f.path, *ln, format!("{} ({h}): {}", f.cell(r, f.key_col), msg(c)))); }
            }
        }
    }
    for f in web_files(ctx) {
        for (i, l) in ctx.read(&f).lines().enumerate() {
            if let Some(c) = first_emoji(l) { out.push(finding(L, &f, (i + 1) as u32, msg(c))); }
        }
    }
    for f in ctx.files("game/", &["tscn", "tres", "gd"]) {
        if exempt(f) { continue; }
        let text = ctx.read(f);
        let code = if f.ends_with(".gd") { strip_gd_comments(&text) } else { text };
        for (i, l) in code.lines().enumerate() {
            if let Some(c) = first_emoji(l) { out.push(finding(L, f, (i + 1) as u32, msg(c))); }
        }
    }
    out
}

// ------------------------------------------------------------------------------------------------
// case
// ------------------------------------------------------------------------------------------------

const SMALL_WORDS: &[&str] = &["a", "an", "the", "and", "or", "nor", "but", "of", "to", "in", "on", "at", "for", "by", "with", "from", "as", "per", "via", "vs", "into", "onto", "off", "up", "if", "is", "be"];

/// Contexts whose English is a proper name (title case allowed).
fn proper_name_context(context: &str) -> bool {
    let c = context.to_ascii_lowercase();
    ["proper noun", "callsign", "ace name", "person name", "place name", "operation name", "region name", "theatre name", "theatre node name", "ship name", "faction name"].iter().any(|p| c.contains(p))
}

fn is_capitalised(w: &str) -> bool {
    let mut cs = w.chars();
    let Some(f) = cs.next() else { return false };
    f.is_uppercase() && cs.clone().any(|c| c.is_lowercase()) && !cs.any(|c| c.is_uppercase())
}

fn is_all_caps(w: &str) -> bool {
    w.chars().filter(|c| c.is_alphabetic()).count() >= 2 && w.chars().all(|c| c.is_uppercase() || c.is_ascii_digit())
}

/// Title Case in one text, after masking. Returns the offending sentence's capitalised words.
fn title_case(masked: &str) -> Option<Vec<String>> {
    for sentence in masked.split(['.', '!', '?', ':', ';', '\n', '(', ')', '"', '—', '–']) {
        let ws: Vec<&str> = sentence.split(|c: char| c.is_whitespace() || c == ',' || c == '/').filter(|w| !w.is_empty()).collect();
        if ws.len() < 2 { continue; }
        let cands: Vec<&str> = ws[1..].iter().copied()
            .map(|w| w.trim_matches(|c: char| !c.is_alphanumeric()))
            .filter(|w| !w.is_empty() && w.chars().all(|c| c.is_alphabetic() || c == '\'' || c == '-') && !w.contains('{'))
            .filter(|w| !SMALL_WORDS.contains(&w.to_lowercase().as_str()))
            .filter(|w| !is_all_caps(w))
            .collect();
        if !cands.is_empty() && cands.iter().all(|w| is_capitalised(w)) {
            return Some(cands.iter().map(|w| w.to_string()).collect());
        }
    }
    None
}

/// All-caps word pairs not covered by the glossary.
fn caps_pairs(masked: &str, glossary: &[String]) -> Vec<String> {
    let toks: Vec<&str> = masked.split(|c: char| c.is_whitespace() || c == '/' || c == ',' || c == '(' || c == ')').filter(|w| !w.is_empty()).collect();
    let mut out = Vec::new();
    for w in toks.windows(2) {
        let (a, b) = (w[0].trim_matches(|c: char| !c.is_alphanumeric()), w[1].trim_matches(|c: char| !c.is_alphanumeric()));
        if !(is_all_caps(a) && is_all_caps(b)) { continue; }
        let pair = format!("{a} {b}");
        let ok = glossary.iter().any(|g| *g == pair) || (glossary.iter().any(|g| g == a) && glossary.iter().any(|g| g == b));
        if !ok { out.push(pair); }
    }
    out
}

/// Title Case strings and all-caps word pairs not in `glossary.txt`. Unit: `xt-lint-copy`.
pub fn case(ctx: &LintCtx) -> Vec<Finding> {
    let glossary = read_list(&ctx.read(GLOSSARY));
    let nouns = capital_words(ctx);
    check_case(&all_copy(ctx), &glossary, &nouns)
}

fn check_case(copy: &[Snippet], glossary: &[String], nouns: &[String]) -> Vec<Finding> {
    const L: &str = "copy.case";
    // Multi-word glossary entries are masked like proper nouns; single codes are checked per pair.
    let mut masks: Vec<String> = nouns.to_vec();
    masks.extend(glossary.iter().filter(|g| g.contains(' ')).cloned());
    masks.sort_by(|a, b| b.len().cmp(&a.len()));
    let mut out = Vec::new();
    for c in copy.iter().filter(|c| !c.text.is_empty()) {
        let masked = mask(&c.text, &masks);
        for pair in caps_pairs(&masked, glossary) {
            out.push(finding(L, &c.path, c.line, format!("\"{pair}\" in capitals; capitals belong to glossary codes (L1607, L1695){}", key_note(c))));
        }
        if c.chatter_or_save() || proper_name_context(&c.context) { continue; }
        if let Some(words) = title_case(&masked) {
            out.push(finding(L, &c.path, c.line, format!("Title Case ({}); use sentence case (L1607, L1695){}", words.join(", "), key_note(c))));
        }
    }
    out
}

// ------------------------------------------------------------------------------------------------
// two_digit
// ------------------------------------------------------------------------------------------------

fn ordered_context(context: &str) -> bool {
    let c = context.to_ascii_lowercase();
    ["tutorial", "briefing", "waypoint", "mission order", "orders", "step"].iter().any(|p| c.contains(p))
}

/// Byte offsets of `(^|\s)0[1-9]\s*[/.\-–]\s` matches that are not dates or bearings.
fn two_digit_hits(text: &str) -> Vec<usize> {
    let chars: Vec<(usize, char)> = text.char_indices().collect();
    let mut out = Vec::new();
    for k in 0..chars.len() {
        let (at, c) = chars[k];
        if c != '0' { continue; }
        if k > 0 && !chars[k - 1].1.is_whitespace() { continue; }
        let Some(&(_, d)) = chars.get(k + 1) else { continue };
        if !('1'..='9').contains(&d) { continue; }
        let mut j = k + 2;
        while j < chars.len() && chars[j].1.is_whitespace() { j += 1; }
        let Some(&(_, sep)) = chars.get(j) else { continue };
        if !matches!(sep, '/' | '.' | '-' | '–') { continue; }
        let Some(&(_, ws)) = chars.get(j + 1) else { continue };
        if !ws.is_whitespace() { continue; }
        // Date: another number group and separator follow ("01 / 05 / 2026").
        let mut m = j + 1;
        while m < chars.len() && chars[m].1.is_whitespace() { m += 1; }
        let digits = chars[m..].iter().take_while(|(_, c)| c.is_ascii_digit()).count();
        let mut n = m + digits;
        while n < chars.len() && chars[n].1.is_whitespace() { n += 1; }
        if digits > 0 && chars.get(n).map_or(false, |&(_, c)| matches!(c, '/' | '.' | '-' | '–')) { continue; }
        // The middle or end of a date: a number and a separator come just before.
        let back = text[..at].trim_end();
        if back.ends_with(['/', '.', '-', '–']) && back[..back.len() - back.chars().next_back().map_or(0, char::len_utf8)].trim_end().ends_with(|c: char| c.is_ascii_digit()) { continue; }
        // Bearing or course ("bearing 05 - ...").
        let before = text[..at].trim_end().to_lowercase();
        let prev = before.split_whitespace().last().unwrap_or("");
        if ["bearing", "brg", "course", "heading", "hdg"].contains(&prev) { continue; }
        out.push(at);
    }
    out
}

/// Regex `(^|\s)0[1-9]\s*[/.\-–]\s` outside dates and bearings, unless the row's context column marks ordered
/// content (tutorial, briefing, waypoint, mission order). Unit: `xt-lint-copy`.
pub fn two_digit(ctx: &LintCtx) -> Vec<Finding> { check_two_digit(&all_copy(ctx)) }

fn check_two_digit(copy: &[Snippet]) -> Vec<Finding> {
    let mut out = Vec::new();
    for c in copy {
        if ordered_context(&c.context) || c.ordered_note { continue; }
        if !two_digit_hits(&c.text).is_empty() {
            out.push(finding("copy.two-digit", &c.path, c.line, format!("two-digit number on a non-sequence (\"01 / ...\"); use plain headings, or mark ordered content{}", key_note(c))));
        }
    }
    out
}

// ------------------------------------------------------------------------------------------------
// sentence_length
// ------------------------------------------------------------------------------------------------

/// Population coefficient of variation.
fn cv(xs: &[f64]) -> f64 {
    let n = xs.len() as f64;
    let mean = xs.iter().sum::<f64>() / n;
    if mean <= 0.0 { return f64::INFINITY; }
    let var = xs.iter().map(|x| (x - mean) * (x - mean)).sum::<f64>() / n;
    var.sqrt() / mean
}

/// (line, word count) per sentence of each section of a web document; sections end at headings.
fn doc_sentences(copy: &[Snippet]) -> Vec<Vec<(u32, usize)>> {
    fn flush(para: &mut Vec<&Snippet>, sec: &mut Vec<(u32, usize)>) {
        if let Some(first) = para.first() {
            let joined = para.iter().map(|c| c.text.as_str()).collect::<Vec<_>>().join(" ");
            for s in sentences(&joined) {
                let w = word_count(&s);
                if w > 0 { sec.push((first.line, w)); }
            }
        }
        para.clear();
    }
    let mut sections: Vec<Vec<(u32, usize)>> = vec![Vec::new()];
    let mut para: Vec<&Snippet> = Vec::new();
    for c in copy {
        if c.heading {
            let sec = sections.last_mut().expect("non-empty");
            flush(&mut para, sec);
            sections.push(Vec::new());
        } else if c.text.is_empty() {
            let sec = sections.last_mut().expect("non-empty");
            flush(&mut para, sec);
        } else {
            para.push(c);
        }
    }
    let sec = sections.last_mut().expect("non-empty");
    flush(&mut para, sec);
    sections
}

/// Web and store copy: 5 consecutive sentences with length CV under 0.40, or 4 consecutive 8-20 word sentences
/// within 5 words of each other. Warn severity until calibrated before 0.1.0 (L1618). Unit: `xt-lint-copy`.
pub fn sentence_length(ctx: &LintCtx) -> Vec<Finding> {
    let mut out = Vec::new();
    for f in web_files(ctx) { out.extend(check_sentence_length(&f, &md_copy(&f, &ctx.read(&f)))); }
    out
}

fn check_sentence_length(path: &str, copy: &[Snippet]) -> Vec<Finding> {
    const L: &str = "copy.sentence-length";
    let warn = |line: u32, msg: String| Finding { lint: L, severity: Severity::Warn, path: path.to_string(), line, message: msg };
    let mut out = Vec::new();
    for sec in doc_sentences(copy) {
        let mut i = 0;
        while i < sec.len() {
            if i + 5 <= sec.len() {
                let xs: Vec<f64> = sec[i..i + 5].iter().map(|s| s.1 as f64).collect();
                let v = cv(&xs);
                if v < 0.40 {
                    out.push(warn(sec[i].0, format!("5 sentences in a row with length CV {v:.2} (under 0.40); vary sentence length (L1618)")));
                    i += 5;
                    continue;
                }
            }
            if i + 4 <= sec.len() {
                let w: Vec<usize> = sec[i..i + 4].iter().map(|s| s.1).collect();
                let (lo, hi) = (*w.iter().min().unwrap_or(&0), *w.iter().max().unwrap_or(&0));
                if lo >= 8 && hi <= 20 && hi - lo <= 5 {
                    out.push(warn(sec[i].0, format!("4 sentences in a row of {lo} to {hi} words; vary sentence length (L1618)")));
                    i += 4;
                    continue;
                }
            }
            i += 1;
        }
    }
    out
}

// ------------------------------------------------------------------------------------------------
// i18n_coverage
// ------------------------------------------------------------------------------------------------

/// A translation key some part of the game uses, and where.
#[derive(Clone, Debug, PartialEq)]
struct Used { key: String, path: String, line: u32 }

fn locate(text: &str, key: &str) -> u32 {
    let q = format!("\"{key}\"");
    text.find(&q).map_or(0, |at| super::godot::line_of(text, at))
}

fn used_from(path: &str, text: &str, keys: impl IntoIterator<Item = String>) -> Vec<Used> {
    keys.into_iter().filter(|k| !k.is_empty()).map(|k| Used { line: locate(text, &k), key: k, path: path.to_string() }).collect()
}

/// Keys from the sim-systems `text_keys()` lists, with the source file of each list.
fn system_keys(ctx: &LintCtx) -> (Vec<Used>, Vec<Finding>) {
    use sim_systems::{editor, graph, objectives, officers, text};
    let lists: [(&str, fn() -> Vec<&'static str>); 8] = [
        ("crates/sim-systems/src/text/mod.rs", text::text_keys),
        ("crates/sim-systems/src/officers/board.rs", officers::board::text_keys),
        ("crates/sim-systems/src/officers/tuning.rs", officers::tuning::text_keys),
        ("crates/sim-systems/src/graph/library.rs", graph::library::text_keys),
        ("crates/sim-systems/src/objectives/mod.rs", objectives::text_keys),
        ("crates/sim-systems/src/editor/notes.rs", editor::notes::text_keys),
        ("crates/sim-systems/src/editor/suggest.rs", editor::suggest::text_keys),
        ("crates/sim-systems/src/editor/splash.rs", editor::splash::text_keys),
    ];
    let mut out = Vec::new();
    let mut bad = Vec::new();
    for (path, list) in lists {
        let src = ctx.read(path);
        match guarded(list) {
            Ok(keys) => out.extend(used_from(path, &src, keys.into_iter().map(str::to_string))),
            Err(e) => bad.push(finding("copy.i18n-coverage", path, 0, format!("its text_keys() list panicked ({e}); its keys cannot be checked"))),
        }
    }
    (out, bad)
}

/// Run `f`, turning a panic (an unfinished or broken key source) into an error so one bad source does not take
/// the whole lint run down. The default panic message is suppressed while `f` runs.
fn guarded<T>(f: impl FnOnce() -> T) -> Result<T, String> {
    let hook = std::panic::take_hook();
    std::panic::set_hook(Box::new(|_| {}));
    let r = std::panic::catch_unwind(std::panic::AssertUnwindSafe(f));
    std::panic::set_hook(hook);
    r.map_err(|p| p.downcast_ref::<&str>().map(|s| s.to_string()).or_else(|| p.downcast_ref::<String>().cloned()).unwrap_or_else(|| "panic".into()))
}

fn kit_keys(k: &content::kit::KitDef) -> Vec<String> {
    let mut v: Vec<String> = Vec::new();
    v.extend(k.name_key.iter().map(|t| t.0.clone()));
    v.extend(k.description_key.iter().map(|t| t.0.clone()));
    v.extend(k.settings.iter().map(|s| s.label.0.clone()));
    v
}

/// Keys in the embedded content data, stock blueprints and kits, and the tutorial script. A file that does not
/// parse is reported, since its keys cannot be checked.
fn content_keys() -> (Vec<Used>, Vec<Finding>) {
    use content::campaign::{AceDef, ActDef, DifficultyPreset, DoctrineCardDef, EnemyClassDef, OperationDef, TheatreDef};
    use content::data::{self, parse_ron};
    use content::part::{weapons::WeaponTables, PartDef};
    const L: &str = "copy.i18n-coverage";
    let mut used = Vec::new();
    let mut bad = Vec::new();
    let mut take = |path: &str, text: &str, r: Result<Vec<String>, String>| match r {
        Ok(keys) => used.extend(used_from(path, text, keys)),
        Err(e) => bad.push(finding(L, path, 0, format!("cannot read its translation keys: {e}"))),
    };
    let err = |e: content::ContentError| e.to_string();
    for (i, &t) in data::PARTS.iter().enumerate() {
        let p = format!("crates/content/data/parts/act{}.ron", i + 1);
        take(&p, t, parse_ron::<Vec<PartDef>>(t, &p).map(|v| v.iter().flat_map(|d| [d.name.0.clone(), d.description.0.clone()]).collect()).map_err(err));
    }
    let p = "crates/content/data/weapons.ron";
    take(p, data::WEAPONS, parse_ron::<WeaponTables>(data::WEAPONS, p).map(|w| w.munitions.iter().map(|m| m.name.0.clone()).collect()).map_err(err));
    let p = "crates/content/data/difficulty.ron";
    take(p, data::DIFFICULTY, parse_ron::<Vec<DifficultyPreset>>(data::DIFFICULTY, p).map(|v| v.iter().map(|d| d.name.0.clone()).collect()).map_err(err));
    let p = "crates/content/data/theatre.ron";
    take(p, data::THEATRE, parse_ron::<TheatreDef>(data::THEATRE, p).map(|t| {
        let mut v = vec![t.name.0.clone()];
        v.extend(t.regions.iter().map(|r| r.name.0.clone()));
        v.extend(t.nodes.iter().map(|n| n.name.0.clone()));
        v
    }).map_err(err));
    let p = "crates/content/data/acts.ron";
    take(p, data::ACTS, parse_ron::<Vec<ActDef>>(data::ACTS, p).map(|v| v.iter().map(|a| a.name.0.clone()).collect()).map_err(err));
    for (i, &t) in data::ENEMY_CLASSES.iter().enumerate() {
        let p = format!("crates/content/data/enemy_classes/act{}.ron", i + 1);
        take(&p, t, parse_ron::<Vec<EnemyClassDef>>(t, &p).map(|v| v.iter().map(|c| c.name.0.clone()).collect()).map_err(err));
    }
    for (i, &t) in data::OPERATIONS.iter().enumerate() {
        let p = format!("crates/content/data/operations/act{}.ron", i + 1);
        take(&p, t, parse_ron::<Vec<OperationDef>>(t, &p).map(|v| v.iter().flat_map(|o| [o.name.0.clone(), o.briefing.0.clone()]).collect()).map_err(err));
    }
    let p = "crates/content/data/aces.ron";
    take(p, data::ACES, parse_ron::<Vec<AceDef>>(data::ACES, p).map(|v| v.iter().flat_map(|a| {
        let mut k = vec![a.name.0.clone(), a.callsign.0.clone()];
        k.extend(a.arcs.iter().flat_map(|arc| arc.lines.iter().map(|l| l.text.0.clone())));
        k
    }).collect()).map_err(err));
    let p = "crates/content/data/doctrine_cards.ron";
    take(p, data::DOCTRINE_CARDS, parse_ron::<Vec<DoctrineCardDef>>(data::DOCTRINE_CARDS, p).map(|v| v.iter().flat_map(|c| [c.name.0.clone(), c.description.0.clone()]).collect()).map_err(err));
    // `blueprints/examples/` are format samples the catalogue never loads, so no player sees their keys.
    for (rel, t) in data::stock_kit_files().filter(|(rel, _)| !rel.starts_with("examples/")) {
        let p = format!("blueprints/{rel}");
        take(&p, t, content::kit::parse_kit(t, &p).map(|k| kit_keys(&k)).map_err(err));
    }
    let catalogue = content::catalogue::Catalogue::load_embedded();
    for (rel, t) in data::stock_blueprint_files().filter(|(rel, _)| !rel.starts_with("examples/")) {
        let p = format!("blueprints/{rel}");
        let r = match &catalogue {
            Ok(cat) => content::blueprint::io::load_blueprint_with(t, &p, cat).map(|l| {
                let bp = l.blueprint;
                let mut v: Vec<String> = bp.header.name_key.iter().map(|k| k.0.clone()).collect();
                v.extend(bp.embedded_kits.values().flat_map(kit_keys));
                v
            }).map_err(err),
            Err(e) => Err(format!("the part catalogue does not load ({e})")),
        };
        take(&p, t, r);
    }
    let p = "crates/sim-systems/data/tutorial.ron";
    let t = sim_systems::tutorial::TUTORIAL_DATA;
    take(p, t, parse_ron::<Vec<sim_systems::tutorial::Script>>(t, p).map(|scripts| {
        let mut v = Vec::new();
        for s in &scripts {
            for step in &s.steps {
                v.push(step.text.0.clone());
                for e in &step.on_enter {
                    if let sim_systems::tutorial::Effect::Show { text, .. } = e { v.push(text.0.clone()); }
                }
            }
        }
        v
    }).map_err(err));
    (used, bad)
}

/// `tr("dotted.key")` in scripts and key-shaped `text` values in scenes.
fn client_keys(ctx: &LintCtx) -> Vec<Used> {
    let mut out = Vec::new();
    for f in ctx.files("game/", &["gd"]) {
        if exempt(f) { continue; }
        let code = strip_gd_comments(&ctx.read(f));
        for (i, line) in code.lines().enumerate() {
            for at in super::godot::find_ident(line, "tr") {
                let rest = line[at + 2..].trim_start();
                let Some(arg) = rest.strip_prefix('(') else { continue };
                let arg = arg.trim_start();
                let Some(q) = arg.chars().next().filter(|c| *c == '"' || *c == '\'') else { continue };
                let Some(end) = arg[1..].find(q) else { continue };
                let k = &arg[1..1 + end];
                if is_key(k) { out.push(Used { key: k.to_string(), path: f.to_string(), line: (i + 1) as u32 }); }
            }
        }
    }
    for f in ctx.files("game/", &["tscn"]) {
        if exempt(f) { continue; }
        for n in scene_nodes(&parse_resource(&ctx.read(f))) {
            for p in SCENE_TEXT_PROPS {
                if let Some(prop) = n.prop(p) {
                    let s = unquote(&prop.value);
                    if prop.value.trim().starts_with('"') && is_key(&s) { out.push(Used { key: s, path: f.to_string(), line: prop.line }); }
                }
            }
        }
    }
    out
}

/// CSV structure: header columns, non-empty English and context, unique keys. Returns the key table.
fn check_csvs(files: &[I18nFile]) -> (Vec<(String, String, u32)>, Vec<Finding>) {
    const L: &str = "copy.i18n-coverage";
    let mut table: Vec<(String, String, u32)> = Vec::new();
    let mut out = Vec::new();
    for f in files {
        if f.key_col != Some(0) { out.push(finding(L, &f.path, 1, "the first column must be \"keys\"")); }
        if f.ctx_col.is_none() { out.push(finding(L, &f.path, 1, "no ?context column; every string carries a context from day one (copy rule 13)")); }
        if f.en_col.is_none() { out.push(finding(L, &f.path, 1, "no en column")); }
        for (ln, r) in &f.rows {
            let key = f.cell(r, f.key_col).trim();
            if key.is_empty() { continue; }
            if f.cell(r, f.en_col).trim().is_empty() { out.push(finding(L, &f.path, *ln, format!("{key} has no English text"))); }
            if f.ctx_col.is_some() && f.cell(r, f.ctx_col).trim().is_empty() { out.push(finding(L, &f.path, *ln, format!("{key} has no context"))); }
            if let Some((_, p, l)) = table.iter().find(|(k, _, _)| k == key) {
                out.push(finding(L, &f.path, *ln, format!("{key} is already defined at {p}:{l}; a key lives in one CSV row")));
            } else {
                table.push((key.to_string(), f.path.clone(), *ln));
            }
        }
    }
    table.sort();
    (table, out)
}

fn check_coverage(used: &[Used], table: &[(String, String, u32)]) -> Vec<Finding> {
    let mut out = Vec::new();
    let mut seen: Vec<(&str, &str)> = Vec::new();
    for u in used {
        if seen.contains(&(u.key.as_str(), u.path.as_str())) { continue; }
        seen.push((u.key.as_str(), u.path.as_str()));
        if table.binary_search_by(|(k, _, _)| k.as_str().cmp(&u.key)).is_err() {
            out.push(finding("copy.i18n-coverage", &u.path, u.line, format!("{} has no row in game/i18n/*.csv (L1707)", u.key)));
        }
    }
    out
}

/// Every translation key the game uses has a row in some `game/i18n/*.csv` with a non-empty English text and
/// context column: keys from every sim-systems `text_keys()` list (text, officers::board, officers::tuning,
/// graph::library, objectives, editor::notes, editor::suggest, editor::splash) and every `TextKey` in the content
/// data (catalogue, kits, campaign, stock blueprints, tutorial). No key appears in two CSVs. Unit: `xt-lint-copy`.
pub fn i18n_coverage(ctx: &LintCtx) -> Vec<Finding> {
    let files = i18n_files(ctx);
    let (table, mut out) = check_csvs(&files);
    let (mut used, bad) = system_keys(ctx);
    out.extend(bad);
    match guarded(content_keys) {
        Ok((content, bad)) => {
            used.extend(content);
            out.extend(bad);
        }
        Err(e) => out.push(finding("copy.i18n-coverage", "crates/content/data", 0, format!("reading the content data's keys panicked ({e}); its keys cannot be checked"))),
    }
    used.extend(client_keys(ctx));
    out.extend(check_coverage(&used, &table));
    out
}

// ------------------------------------------------------------------------------------------------
// Tests
// ------------------------------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    fn csv_row(text: &str, context: &str) -> Snippet {
        Snippet { path: "game/i18n/x.csv".into(), line: 2, kind: Kind::Csv, key: "k.x".into(), context: context.into(), text: text.into(), heading: false, button: context.contains("button"), ordered_note: false }
    }

    #[test]
    fn csv_parser_quotes_newlines_crlf() {
        let t = "keys,?context,en\r\na.b,\"Ctx, with comma\",\"Say \"\"hi\"\"\"\r\nc.d,ctx,\"two\nlines\"\ne.f,,\n";
        let rows = parse_csv(t);
        assert_eq!(rows.len(), 4);
        assert_eq!(rows[1].1, vec!["a.b", "Ctx, with comma", "Say \"hi\""]);
        assert_eq!(rows[2].1[2], "two\nlines");
        assert_eq!(rows[3].0, 5);
        assert_eq!(rows[3].1, vec!["e.f", "", ""]);
        let f = I18nFile::parse("game/i18n/x.csv", t);
        assert_eq!((f.key_col, f.ctx_col, f.en_col), (Some(0), Some(1), Some(2)));
    }

    #[test]
    fn banned_words_and_buttons() {
        let banned = read_list("# c\nunlock\nnext-gen\nget started\nsmart\n");
        let nouns = vec!["Smart App Control".to_string()];
        let copy = vec![
            csv_row("Unlock the gun", "Toast"),
            csv_row("Unlocked doors", "Toast"),
            csv_row("A next-gen hull", "Toast"),
            csv_row("Smart App Control blocks it", "Toast"),
            csv_row("Get started", "Menu button"),
            csv_row("Launch decoys →", "Menu button"),
            csv_row("Turn to port ←", "Menu button"),
            csv_row("Fire!", "Menu button"),
            csv_row("Unlock this", "Radio line, Helm"),
        ];
        let f = check_banned(&copy, &banned, &nouns);
        // unlock, next-gen, get started, arrow, exclamation.
        assert_eq!(f.len(), 5, "{f:#?}");
    }

    #[test]
    fn copy_tics() {
        assert!(!tics("It's not just a boat, it's a fleet.").is_empty());
        assert!(!tics("Build. Fight. Command.").is_empty());
        assert!(!tics("PROPULSION — engines and shafts").is_empty());
        assert!(!tics("Fast · Deadly · Yours").is_empty());
        assert!(!tics("One — two — three").is_empty());
        assert!(tics("Holding course 040 at 12 kn.").is_empty());
        assert!(tics("Track 0412 is hostile.").is_empty());
    }

    #[test]
    fn emoji_ranges() {
        assert_eq!(first_emoji("Fire 🔥"), Some('🔥'));
        assert_eq!(first_emoji("Ok ✓"), Some('✓'));
        assert_eq!(first_emoji("ÅÉÎÕÜ Çgjpqy µ – →"), None);
    }

    #[test]
    fn case_rules() {
        let glossary = read_list("EMCON\nESM\nID\nCC BY\nMIT\n");
        let nouns = vec!["Black Pennant".to_string(), "Hostile".to_string(), "Damage Control".to_string()];
        let copy = vec![
            csv_row("Launch Decoys", "Button"),
            csv_row("Fire Control Radar", "Panel title"),
            csv_row("EMCON setting", "Row"),
            csv_row("Track 0412, Hostile, bearing 047", "Track"),
            csv_row("Black Pennant is a builder", "Blurb"),
            csv_row("Call Damage Control", "Order"),
            csv_row("ALL STOP now", "Order"),
            csv_row("ESM EMCON check", "Order"),
            csv_row("Licensed CC BY 4.0", "Blurb"),
            csv_row("Holding: track is {0}, doctrine needs {1}", "Hold reason"),
            csv_row("Iron Gate", "Operation name"),
            csv_row("Engaging Track", "Radio line, Gunnery"),
        ];
        let f = check_case(&copy, &glossary, &nouns);
        // two Title Case rows, one caps pair.
        assert_eq!(f.len(), 3, "{f:#?}");
    }

    #[test]
    fn two_digit_regex() {
        assert_eq!(two_digit_hits("01 / Features").len(), 1);
        assert_eq!(two_digit_hits("Step 02. Build").len(), 1);
        assert_eq!(two_digit_hits("03 – Fight").len(), 1);
        assert!(two_digit_hits("01 / 05 / 2026").is_empty());
        assert!(two_digit_hits("bearing 05 - closing").is_empty());
        assert!(two_digit_hits("Track 0412 / hostile").is_empty());
        assert!(two_digit_hits("10 / 20").is_empty());
        let copy = vec![csv_row("01 / Situation", "Briefing heading"), csv_row("01 / Features", "Menu")];
        assert_eq!(check_two_digit(&copy).len(), 1);
    }

    #[test]
    fn markdown_prose_extraction() {
        let md = "# Black Pennant\n\nSome `code SMART` and [a link](https://x.y/z) here.\n\n```\nsmart unlock\n```\n<!-- a\nb -->\n- Item one\n| a | b |\n|---|---|\n";
        let c = md_copy("README.md", md);
        let texts: Vec<&str> = c.iter().filter(|c| !c.text.is_empty()).map(|c| c.text.as_str()).collect();
        assert_eq!(texts, vec!["Black Pennant", "Some   and a link here.", "Item one", "a   b"]);
        assert!(c[0].heading);
    }

    #[test]
    fn sentence_length_windows() {
        let even = "This sentence has exactly nine words in it now. This sentence has exactly nine words in it too. This sentence has exactly ten words in it as well. This sentence has exactly nine words in it again. This one has nine words in it as well ok.";
        let f = check_sentence_length("README.md", &md_copy("README.md", even));
        assert_eq!(f.len(), 1);
        assert_eq!(f[0].severity, Severity::Warn);
        let varied = "Short one. This sentence is quite a bit longer than the first one was. Tiny. Another sentence of moderate length follows here. End.";
        assert!(check_sentence_length("README.md", &md_copy("README.md", varied)).is_empty());
        assert!((cv(&[10.0, 10.0, 10.0]) - 0.0).abs() < 1e-12);
    }

    #[test]
    fn coverage_core() {
        let a = I18nFile::parse("game/i18n/a.csv", "keys,?context,en\na.one,Ctx,One\na.two,,Two\na.three,Ctx,\n");
        let b = I18nFile::parse("game/i18n/b.csv", "keys,en\na.one,Again\n");
        let (table, f) = check_csvs(&[a, b]);
        // a.two no context, a.three no English, b has no context column, a.one duplicated.
        assert_eq!(f.len(), 4, "{f:#?}");
        let used = vec![
            Used { key: "a.one".into(), path: "src.rs".into(), line: 3 },
            Used { key: "a.missing".into(), path: "src.rs".into(), line: 4 },
            Used { key: "a.missing".into(), path: "src.rs".into(), line: 4 },
        ];
        let f = check_coverage(&used, &table);
        assert_eq!(f.len(), 1);
        assert_eq!(f[0].line, 4);
        assert!(is_key("sim.hold.no_target"));
        assert!(!is_key("Holding: no target"));
        assert!(!is_key("v1.2"));
        assert_eq!(locate("x\nconst K: &[&str] = &[\"a.b\"];\n", "a.b"), 2);
    }

    #[test]
    fn masking_keeps_offsets() {
        let m = mask("Use Smart App Control now", &["Smart App Control".to_string()]);
        assert_eq!(m.len(), "Use Smart App Control now".len());
        assert!(!m.contains("Smart"));
        assert_eq!(mask("Hostiles", &["Hostile".to_string()]), "Hostiles");
    }
}

#[cfg(test)]
mod guard_tests {
    #[test]
    fn guarded_turns_panics_into_errors() {
        assert_eq!(super::guarded(|| 3), Ok(3));
        let e = super::guarded(|| -> u8 { panic!("unit: x") }).unwrap_err();
        assert!(e.contains("unit: x"));
    }
}
