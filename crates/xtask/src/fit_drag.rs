//! Unit `xt-fit-drag`. `cargo xtask fit-drag [--check]`. DESIGN.md L563-570: each calibre band's quadratic drag
//! coefficient is fitted so the impact velocity at the band's cap range turns `p_1km_mm` into `p_cap_mm`; CI
//! fails if the committed k misses by over 5%. The fit itself is `sim_core::projectiles::fit_drag_k`.
//!
//! Reads `crates/content/data/weapons.ron` (parsed with `content::data::parse_ron` into
//! `content::part::weapons::WeaponTables`), fits every band, and rewrites only the `drag_k:` value of each band
//! row in place, preserving the rest of the text and comments. `--check` writes nothing and fails when any
//! committed k differs from the fit by more than 5%.
//!
//! The rewrite works on a small RON tokenizer (comments, strings, brackets, scalars) rather than on a
//! re-serialisation, so comments and layout survive. It locates the top-level `bands: [...]` list, and in each
//! row the `band:` name and the `drag_k:` value span. The rewritten text is parsed again and compared with the
//! original table before it is written, so a rewrite can never change anything but the k values.
//!
//! The committed table is also checked by this crate's unit tests, so `cargo test --workspace` in CI enforces
//! the 5% gate even where the workflow does not call `fit-drag --check` itself.
use std::ops::Range;
use std::path::Path;

use content::part::weapons::{BandDef, CalibreBand, WeaponTables};

/// Relative path of the weapons table.
pub const WEAPONS_RON: &str = "crates/content/data/weapons.ron";
/// Allowed relative miss before CI fails (L563).
pub const TOLERANCE: f64 = 0.05;

/// Unit: `xt-fit-drag`.
pub fn run(root: &Path, check: bool) -> Result<(), String> {
    let path = root.join(WEAPONS_RON);
    let text = std::fs::read_to_string(&path).map_err(|e| format!("cannot read {}: {e}", path.display()))?;
    let report = fit_text(&text)?;
    for line in &report.lines { println!("{line}"); }
    if check {
        if report.misses.is_empty() {
            println!("fit-drag: all {} bands within {:.0}% of the fit", report.fits.len(), TOLERANCE * 100.0);
            return Ok(());
        }
        return Err(format!(
            "{} band(s) miss the fitted drag k by more than {:.0}%: {}. Run `cargo xtask fit-drag` and commit {WEAPONS_RON}",
            report.misses.len(),
            TOLERANCE * 100.0,
            report.misses.join(", ")
        ));
    }
    if report.rewritten == text {
        println!("fit-drag: {WEAPONS_RON} is up to date");
        return Ok(());
    }
    write_atomic(&path, &report.rewritten)?;
    println!("fit-drag: wrote {WEAPONS_RON}");
    Ok(())
}

/// Result of fitting a weapons table text.
#[derive(Debug)]
struct FitReport {
    /// (band, committed k, fitted k as written) in table order.
    fits: Vec<(CalibreBand, f64, f64)>,
    /// Bands whose committed k misses the fit by more than [`TOLERANCE`].
    misses: Vec<String>,
    /// One human-readable line per band.
    lines: Vec<String>,
    /// The table text with every out-of-date `drag_k` replaced by its fit (identical to the input when current).
    rewritten: String,
}

/// Parse, validate, fit every band and build the rewritten text. Writes nothing.
fn fit_text(text: &str) -> Result<FitReport, String> {
    let tables: WeaponTables = content::data::parse_ron(text, WEAPONS_RON).map_err(|e| e.to_string())?;
    validate_band_set(&tables.bands)?;
    let rows = locate_band_rows(text)?;
    if rows.len() != tables.bands.len() {
        return Err(format!("found {} band rows in the text but the table parsed {}", rows.len(), tables.bands.len()));
    }
    for (row, band) in rows.iter().zip(&tables.bands) {
        if row.band != format!("{:?}", band.band) {
            return Err(format!("band row order mismatch: text has {} where the table parsed {:?}", row.band, band.band));
        }
    }

    let mut fits = Vec::with_capacity(tables.bands.len());
    let mut misses = Vec::new();
    let mut lines = Vec::new();
    let mut new_values = Vec::with_capacity(tables.bands.len());
    for band in &tables.bands {
        let fitted = sim_core::projectiles::fit_drag_k(band);
        if !(fitted.is_finite() && fitted > 0.0) {
            return Err(format!(
                "band {:?}: cannot fit drag k (needs finite positive p_1km_mm, cap_range_m and ref_muzzle_mps, \
                 0 < p_cap_mm < p_1km_mm, and a cap range the reference shell can reach)",
                band.band
            ));
        }
        let formatted = format_k(fitted);
        let written: f64 = formatted.parse().map_err(|_| format!("band {:?}: formatted k {formatted} does not parse", band.band))?;
        let committed = band.drag_k;
        let miss = relative_miss(committed, written);
        let ok = miss <= TOLERANCE;
        lines.push(format!(
            "{:<7} committed {:<14} fit {:<14} miss {:>8}{}",
            format!("{:?}", band.band),
            format_k(committed),
            formatted,
            if miss.is_finite() { format!("{:.3}%", miss * 100.0) } else { "unusable".to_string() },
            if ok { "" } else { "  over tolerance" }
        ));
        if !ok { misses.push(format!("{:?}", band.band)); }
        fits.push((band.band, committed, written));
        new_values.push(formatted);
    }

    // Replace spans back to front so earlier offsets stay valid. A value that already reads as the fit keeps
    // its original spelling, so a run over an up-to-date table is byte-identical.
    let mut rewritten = text.to_string();
    for (i, row) in rows.iter().enumerate().rev() {
        if text[row.k_span.clone()].parse::<f64>().ok() == Some(fits[i].2) { continue; }
        rewritten.replace_range(row.k_span.clone(), &new_values[i]);
    }

    // The rewrite must change nothing but the k values.
    if rewritten != text {
        let reparsed: WeaponTables =
            content::data::parse_ron(&rewritten, WEAPONS_RON).map_err(|e| format!("rewritten table does not parse: {e}"))?;
        let mut expected = tables.clone();
        for (b, f) in expected.bands.iter_mut().zip(&fits) { b.drag_k = f.2; }
        if reparsed != expected {
            return Err("rewritten table differs from the original beyond the drag_k values; nothing written".to_string());
        }
    }
    Ok(FitReport { fits, misses, lines, rewritten })
}

/// Every band exactly once: a missing band silently falls back to the spec row at runtime, and a duplicate is
/// ambiguous (the runtime takes the first).
fn validate_band_set(bands: &[BandDef]) -> Result<(), String> {
    let mut problems = Vec::new();
    for b in CalibreBand::ALL {
        match bands.iter().filter(|r| r.band == b).count() {
            0 => problems.push(format!("band {b:?} is missing")),
            1 => {}
            n => problems.push(format!("band {b:?} appears {n} times")),
        }
    }
    if problems.is_empty() { Ok(()) } else { Err(problems.join("; ")) }
}

/// Relative difference of the committed k from the fit; infinite when either value is unusable.
fn relative_miss(committed: f64, fitted: f64) -> f64 {
    if !(committed.is_finite() && committed > 0.0 && fitted.is_finite() && fitted > 0.0) { return f64::INFINITY; }
    (committed - fitted).abs() / fitted
}

/// Seven significant figures in the table's existing style, e.g. `3.263095e-4`: far finer than the 5% gate,
/// and identical on every platform because the fit is libm-only and deterministic.
fn format_k(k: f64) -> String { format!("{k:.6e}") }

/// Write through a sibling temporary file and rename, so an interrupted run never leaves a torn table.
fn write_atomic(path: &Path, text: &str) -> Result<(), String> {
    let tmp = path.with_extension("ron.fit-drag.tmp");
    std::fs::write(&tmp, text).map_err(|e| format!("cannot write {}: {e}", tmp.display()))?;
    std::fs::rename(&tmp, path).map_err(|e| {
        let _ = std::fs::remove_file(&tmp);
        format!("cannot replace {}: {e}", path.display())
    })
}

// ---------------------------------------------------------------------------------------------------------
// Minimal RON tokenizer and band-row locator.

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Tok { Ident, Scalar, Str, Open(u8), Close(u8), Colon, Comma, Other }

#[derive(Clone, Debug)]
struct Token { tok: Tok, span: Range<usize> }

/// One band row in the text.
#[derive(Clone, Debug, PartialEq)]
struct BandRow {
    /// The `band:` variant name as written.
    band: String,
    /// Byte span of the `drag_k` value.
    k_span: Range<usize>,
}

fn tokenize(text: &str) -> Result<Vec<Token>, String> {
    let b = text.as_bytes();
    let n = b.len();
    let mut out = Vec::new();
    let mut i = 0;
    let line_of = |at: usize| text[..at].bytes().filter(|&c| c == b'\n').count() + 1;
    while i < n {
        let c = b[i];
        if c.is_ascii_whitespace() { i += 1; continue; }
        let next = b.get(i + 1).copied();
        if c == b'/' && next == Some(b'/') {
            while i < n && b[i] != b'\n' { i += 1; }
            continue;
        }
        if c == b'/' && next == Some(b'*') {
            let start = i;
            let mut depth = 0usize;
            loop {
                if i + 1 >= n { return Err(format!("unterminated block comment at line {}", line_of(start))); }
                if b[i] == b'/' && b[i + 1] == b'*' {
                    depth += 1;
                    i += 2;
                } else if b[i] == b'*' && b[i + 1] == b'/' {
                    depth -= 1;
                    i += 2;
                    if depth == 0 { break; }
                } else {
                    i += 1;
                }
            }
            continue;
        }
        let start = i;
        // Raw strings: r"..." or r#"..."# with any number of hashes.
        if c == b'r' && matches!(next, Some(b'"') | Some(b'#')) {
            let mut j = i + 1;
            let mut hashes = 0usize;
            while j < n && b[j] == b'#' { hashes += 1; j += 1; }
            if j < n && b[j] == b'"' {
                j += 1;
                loop {
                    if j >= n { return Err(format!("unterminated raw string at line {}", line_of(start))); }
                    if b[j] == b'"' && j + hashes < n && b[j + 1..=j + hashes].iter().all(|&h| h == b'#') {
                        j += 1 + hashes;
                        break;
                    }
                    j += 1;
                }
                out.push(Token { tok: Tok::Str, span: start..j });
                i = j;
                continue;
            }
        }
        if c == b'"' || c == b'\'' {
            i += 1;
            loop {
                if i >= n { return Err(format!("unterminated string at line {}", line_of(start))); }
                if b[i] == b'\\' { i += 2; continue; }
                if b[i] == c { i += 1; break; }
                i += 1;
            }
            out.push(Token { tok: Tok::Str, span: start..i.min(n) });
            continue;
        }
        let signed = (c == b'-' || c == b'+') && next.is_some_and(|d| d.is_ascii_alphanumeric() || d == b'.');
        if c.is_ascii_digit() || signed || (c == b'.' && next.is_some_and(|d| d.is_ascii_digit())) {
            i += 1;
            while i < n {
                let d = b[i];
                let exp_sign = (d == b'-' || d == b'+') && matches!(b[i - 1], b'e' | b'E') && !text[start..i].contains(['x', 'X']);
                if d.is_ascii_alphanumeric() || d == b'.' || d == b'_' || exp_sign { i += 1; } else { break; }
            }
            out.push(Token { tok: Tok::Scalar, span: start..i });
            continue;
        }
        if c.is_ascii_alphabetic() || c == b'_' {
            while i < n && (b[i].is_ascii_alphanumeric() || b[i] == b'_') { i += 1; }
            out.push(Token { tok: Tok::Ident, span: start..i });
            continue;
        }
        let tok = match c {
            b'(' | b'[' | b'{' => Tok::Open(c),
            b')' => Tok::Close(b'('),
            b']' => Tok::Close(b'['),
            b'}' => Tok::Close(b'{'),
            b':' => Tok::Colon,
            b',' => Tok::Comma,
            _ => Tok::Other,
        };
        // Step over one whole UTF-8 character so spans stay on char boundaries.
        i += text[i..].chars().next().map(char::len_utf8).unwrap_or(1);
        out.push(Token { tok, span: start..i });
    }
    Ok(out)
}

/// Index of the token closing the bracket opened at `open`.
fn matching_close(toks: &[Token], open: usize) -> Result<usize, String> {
    let mut stack = Vec::new();
    for (j, t) in toks.iter().enumerate().skip(open) {
        match t.tok {
            Tok::Open(c) => stack.push(c),
            Tok::Close(c) => {
                if stack.pop() != Some(c) { return Err("mismatched brackets".to_string()); }
                if stack.is_empty() { return Ok(j); }
            }
            _ => {}
        }
    }
    Err("unclosed bracket".to_string())
}

/// `key: value` pairs directly inside the brackets `open..close` (nested brackets skipped), as
/// (key, index of the value's first token).
fn fields(text: &str, toks: &[Token], open: usize, close: usize) -> Vec<(String, usize)> {
    let mut out = Vec::new();
    let mut depth = 0usize;
    for j in open + 1..close {
        match toks[j].tok {
            Tok::Open(_) => depth += 1,
            Tok::Close(_) => depth = depth.saturating_sub(1),
            Tok::Ident if depth == 0 && j + 2 < close && toks[j + 1].tok == Tok::Colon => {
                out.push((text[toks[j].span.clone()].to_string(), j + 2));
            }
            _ => {}
        }
    }
    out
}

/// Locate every row of the top-level `bands` list.
fn locate_band_rows(text: &str) -> Result<Vec<BandRow>, String> {
    let toks = tokenize(text)?;
    // An optional struct name before the outer parenthesis.
    let root = usize::from(toks.first().map(|t| t.tok) == Some(Tok::Ident));
    if toks.get(root).map(|t| t.tok) != Some(Tok::Open(b'(')) {
        return Err("weapons table does not start with a struct".to_string());
    }
    let root_close = matching_close(&toks, root)?;
    let (_, list) = fields(text, &toks, root, root_close)
        .into_iter()
        .find(|(k, _)| k == "bands")
        .ok_or("no top-level `bands` field")?;
    if toks[list].tok != Tok::Open(b'[') { return Err("`bands` is not a list".to_string()); }
    let list_close = matching_close(&toks, list)?;

    let mut rows = Vec::new();
    let mut j = list + 1;
    while j < list_close {
        match toks[j].tok {
            Tok::Comma => j += 1,
            // A struct name before a row's parenthesis.
            Tok::Ident if toks[j + 1].tok == Tok::Open(b'(') => j += 1,
            Tok::Open(b'(') => {
                let close = matching_close(&toks, j)?;
                let fs = fields(text, &toks, j, close);
                let find = |key: &str| fs.iter().find(|(k, _)| k == key).map(|(_, v)| *v);
                let band = find("band").ok_or("a band row has no `band` field")?;
                if toks[band].tok != Tok::Ident { return Err("a band row's `band` is not a name".to_string()); }
                let name = text[toks[band].span.clone()].to_string();
                let k = find("drag_k").ok_or(format!("band {name} has no `drag_k` field"))?;
                if !matches!(toks[k].tok, Tok::Scalar | Tok::Ident) { return Err(format!("band {name}: `drag_k` is not a number")); }
                rows.push(BandRow { band: name, k_span: toks[k].span.clone() });
                j = close + 1;
            }
            _ => return Err("unexpected token in the `bands` list".to_string()),
        }
    }
    Ok(rows)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn row(band: &str, k: &str) -> String {
        let (p1, pc, cap, v0): (f64, f64, f64, f64) = match band {
            "Auto" => (20.0, 8.0, 3000.0, 1050.0),
            "Medium" => (45.0, 25.0, 8000.0, 925.0),
            "Large" => (90.0, 55.0, 12000.0, 850.0),
            _ => (150.0, 100.0, 18000.0, 880.0),
        };
        format!(
            "(band: {band}, p_1km_mm: {p1:?}, p_cap_mm: {pc:?}, cap_range_m: {cap:?}, fuse_delay_cells: 1, path_k: 2, \
             he_b: 4, he_r_cells: 1, ref_muzzle_mps: {v0:?}, drag_k: {k})"
        )
    }

    fn table(ks: [&str; 4]) -> String {
        let rows: Vec<String> = ["Auto", "Medium", "Large", "Heavy"]
            .iter()
            .zip(ks)
            .map(|(n, k)| format!("        // row {n}: drag_k: 9.9\n        {},", row(n, k)))
            .collect();
        format!(
            "// header, bands: [ (drag_k: 1.0) ]\n/* block /* nested */ drag_k: 2.0 */\n(\n    bands: [\n{}\n    ],\n    munitions: [],\n)\n",
            rows.join("\n")
        )
    }

    /// Fitted k of each spec row, computed once for the tests that compare against it.
    fn spec_fits() -> &'static [f64; 4] {
        static FITS: std::sync::OnceLock<[f64; 4]> = std::sync::OnceLock::new();
        FITS.get_or_init(|| CalibreBand::ALL.map(|b| format_k(sim_core::projectiles::fit_drag_k(&BandDef::spec_default(b))).parse().unwrap()))
    }

    #[test]
    fn locates_rows_past_comments_and_strings() {
        let text = table(["1.0e-4", "2.0e-5", "3e-5", "-1"]);
        let rows = locate_band_rows(&text).unwrap();
        let names: Vec<&str> = rows.iter().map(|r| r.band.as_str()).collect();
        assert_eq!(names, ["Auto", "Medium", "Large", "Heavy"]);
        let vals: Vec<&str> = rows.iter().map(|r| &text[r.k_span.clone()]).collect();
        assert_eq!(vals, ["1.0e-4", "2.0e-5", "3e-5", "-1"]);
        // Strings and raw strings holding brackets do not confuse the bracket matcher; struct names are skipped.
        let t2 = "WeaponTables(munitions: [(name: \"a)]\\\"\", note: r#\"x\")\"#)], bands: [CalibreRow(band: Auto, drag_k: 5.0)])";
        let rows = locate_band_rows(t2).unwrap();
        assert_eq!(rows.len(), 1);
        assert_eq!(&t2[rows[0].k_span.clone()], "5.0");
        assert!(locate_band_rows("(bands: [(band: Auto)])").is_err());
        assert!(locate_band_rows("(bands: [(band: Auto, drag_k: 1.0)").is_err());
        assert!(locate_band_rows("(munitions: [])").is_err());
        assert!(locate_band_rows("(bands: [(band: Auto, drag_k: \"x\")])").is_err());
        assert!(locate_band_rows("/* open").is_err());
        assert!(locate_band_rows("").is_err());
    }

    #[test]
    fn rewrite_changes_only_k_and_check_catches_misses() {
        let text = table(["1.0e-4", "1.0e-4", "1.0e-4", "1.0e-4"]);
        let r = fit_text(&text).unwrap();
        assert_eq!(r.misses.len(), 4);
        // Only the four values changed: comments, decoys and layout survive.
        assert!(r.rewritten.contains("// row Auto: drag_k: 9.9") && r.rewritten.contains("drag_k: 2.0 */"));
        assert_eq!(r.rewritten.lines().count(), text.lines().count());
        let ks: Vec<f64> = r.fits.iter().map(|f| f.2).collect();
        assert_eq!(ks, spec_fits().to_vec());
        // Fitting the rewritten text is a fixed point and passes the check.
        let again = fit_text(&r.rewritten).unwrap();
        assert!(again.misses.is_empty());
        assert_eq!(again.rewritten, r.rewritten);
        // A 4% miss passes, a 6% miss fails, and a passing but inexact value is still refreshed on write.
        let (a, b, c, d) = (format_k(ks[0] * 1.04), format_k(ks[1] * 0.94), format_k(ks[2]), format_k(ks[3]));
        let mixed = fit_text(&table([&a, &b, &c, &d])).unwrap();
        assert_eq!(mixed.misses, ["Medium"]);
        assert_eq!(mixed.rewritten, r.rewritten);
    }

    #[test]
    fn bad_tables_are_errors_not_panics() {
        assert!(fit_text("not ron").is_err());
        let base = table(["1e-4", "1e-4", "1e-4", "1e-4"]);
        let three = base.replace(&format!("{},", row("Heavy", "1e-4")), "");
        assert!(fit_text(&three).unwrap_err().contains("Heavy is missing"));
        let dup = base.replace(&row("Heavy", "1e-4"), &row("Large", "1e-4"));
        assert!(fit_text(&dup).unwrap_err().contains("Large appears 2 times"));
        // Unfittable row: far-range target not below the 1 km value.
        let bad = base.replace("p_cap_mm: 8.0", "p_cap_mm: 30.0");
        assert!(fit_text(&bad).unwrap_err().contains("Auto"));
        assert_eq!(relative_miss(f64::NAN, 1.0), f64::INFINITY);
        assert_eq!(relative_miss(0.0, 1.0), f64::INFINITY);
        assert!((relative_miss(0.95, 1.0) - 0.05).abs() < 1e-12);
    }

    /// The CI gate (L563): the committed weapons table is within 5% of the fit.
    #[test]
    fn committed_weapons_table_is_within_tolerance() {
        let r = fit_text(content::data::WEAPONS).unwrap();
        assert!(r.misses.is_empty(), "drag k out of tolerance for {:?}; run `cargo xtask fit-drag`\n{}", r.misses, r.lines.join("\n"));
    }

    #[test]
    fn run_writes_and_checks_a_repo_copy() {
        let dir = std::env::temp_dir().join(format!("bp-fit-drag-{}", std::process::id()));
        let file = dir.join(WEAPONS_RON);
        std::fs::create_dir_all(file.parent().unwrap()).unwrap();
        std::fs::write(&file, table(["1e-4", "1e-4", "1e-4", "1e-4"])).unwrap();
        assert!(run(&dir, true).is_err());
        run(&dir, false).unwrap();
        run(&dir, true).unwrap();
        let before = std::fs::read_to_string(&file).unwrap();
        run(&dir, false).unwrap();
        assert_eq!(std::fs::read_to_string(&file).unwrap(), before);
        assert!(run(&dir.join("nowhere"), true).is_err());
        let _ = std::fs::remove_dir_all(&dir);
    }
}
