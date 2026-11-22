//! Unit `data-enemy-act1`: the Combine's Act 1 classes are real blueprints, built by the editor's steps in
//! `combine/builder.rs` from the build sheets in `combine/classes.rs`, and committed under
//! `blueprints/combine/act1/`. Regenerate them with
//! `cargo test -p content --test combine_act1 -- --ignored write_combine_act1`.
mod combine {
    pub mod builder;
    pub mod classes;
}
use combine::builder::{build, gm_floor, Built, TRIM_LIMIT_DEG};
use combine::classes::ALL;
use content::blueprint::BlueprintKind;
use content::campaign::{ClassRole, EnemyClassDef};
use content::catalogue::Catalogue;
use content::data::{self, parse_ron};
use content::part::PartFunction;

fn cat() -> Catalogue { Catalogue::load_embedded().expect("embedded catalogue loads") }

/// Validation in enemy-class mode under the open campaign ruleset with no tonnage cap.
fn issues(bp: &content::blueprint::Blueprint, cat: &Catalogue) -> Vec<content::validate::Issue> {
    let rs = content::ruleset::Ruleset { tonnage_cap_t: 0.0, ..content::ruleset::Ruleset::campaign(content::enums::Act::Act1) };
    let ctx = content::validate::ValidationContext { ruleset: &rs, available: None, enemy_class: true, complexity: None };
    content::validate::validate_blueprint(bp, cat, &ctx).issues
}

fn out_dir() -> std::path::PathBuf { std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../blueprints/combine/act1") }

fn table(built: &[(&str, Built)]) -> String {
    let mut s = String::from("class               t      light  cost  draft  dmax  deck_t  fb    GM     trim  deg    kW+    kW-   shaft\n");
    for (f, b) in built {
        let r = &b.report;
        s += &format!("{f:18} {:6.1} {:6.1} {:5} {:5.2} {:5.2} {:7.1} {:5.2} {:6.2} {:+6.3} {:+5.2} {:6.0} {:6.0} {:6.0}\n",
            r.stats.tonnage_t, r.stats.light_t, r.stats.cost, r.draft_m, r.draft_max_m, r.displacement_at_deck_t, r.freeboard_m, r.gm_m, r.trim_lever, r.trim_deg, r.power_kw, r.load_kw, r.shaft_kw);
    }
    s
}

/// Every class built once per test process (the build sheets are deterministic).
fn built() -> &'static [Built] {
    static BUILT: std::sync::OnceLock<Vec<Built>> = std::sync::OnceLock::new();
    BUILT.get_or_init(|| { let cat = cat(); ALL.iter().map(|s| build(s, &cat)).collect() })
}

fn classes() -> Vec<EnemyClassDef> { parse_ron(data::ENEMY_CLASSES[0], "enemy_classes/act1.ron").unwrap() }

fn committed(file: &str) -> String {
    // A Windows checkout may carry CRLF line ends; the generator writes LF.
    std::fs::read_to_string(out_dir().join(format!("{file}.bp.ron"))).unwrap_or_else(|e| panic!("{file}.bp.ron: {e}")).replace("\r\n", "\n")
}

/// The committed files are exactly what the build sheets produce: no hand edits drift from the generator.
#[test]
fn committed_classes_match_their_build_sheets() {
    let mut stale = Vec::new();
    for (spec, b) in ALL.iter().zip(built()) {
        assert!(b.report.errors.is_empty(), "{:?}", b.report.errors);
        if content::blueprint::io::to_file_text(&b.blueprint) != committed(spec.file) { stale.push(spec.file); }
    }
    assert!(stale.is_empty(), "stale class blueprints {stale:?}: run `cargo test -p content --test combine_act1 -- --ignored write_combine_act1`");
}

/// Every class row names a committed blueprint that loads through the stock loader, validates in enemy-class
/// mode with no issue, and carries the class row's name key.
#[test]
fn every_class_is_a_valid_stock_blueprint() {
    let cat = cat();
    let rows = classes();
    assert_eq!(rows.len(), ALL.len(), "one build sheet per class row");
    for row in &rows {
        let file = row.blueprint.strip_prefix("combine/act1/").and_then(|f| f.strip_suffix(".bp.ron")).unwrap_or_else(|| panic!("{}: path {}", row.id, row.blueprint));
        let spec = ALL.iter().find(|s| s.file == file).unwrap_or_else(|| panic!("{}: no build sheet {file}", row.id));
        let bp = content::campaign::load::stock_blueprint(&row.blueprint).unwrap_or_else(|e| panic!("{}: {e}", row.id));
        assert_eq!(bp.header.kind, BlueprintKind::EnemyClass, "{}", row.id);
        assert_eq!(bp.header.name_key.as_ref(), Some(&row.name), "{}", row.id);
        assert_eq!(row.name.as_str(), spec.key);
        let issues = issues(&bp, &cat);
        assert!(issues.is_empty(), "{}: {issues:?}", row.id);
        let st = content::stats::compute_stats(&bp, &cat);
        assert!(st.length_m <= 90.0, "{} is {} m; capitals over 90 m must say so", row.id, st.length_m);
        assert!(!row.capital && !row.submersible && row.air_wing.is_empty(), "{}", row.id);
    }
    // The classes span the five tiers (L756) and the roles the theatre needs.
    for t in [content::enums::Tier::T1, content::enums::Tier::T2, content::enums::Tier::T3, content::enums::Tier::T4, content::enums::Tier::T5] {
        assert!(rows.iter().any(|c| c.tier == t), "tier {t:?} has no class");
    }
    for role in [ClassRole::Freighter, ClassRole::Tanker, ClassRole::Tender, ClassRole::Civilian, ClassRole::PatrolBoat, ClassRole::FastAttack, ClassRole::Corvette, ClassRole::Frigate, ClassRole::Picket] {
        assert!(rows.iter().any(|c| c.role == role), "no {role:?} class");
    }
    assert!((18..=24).contains(&rows.len()), "about 20 classes at the first release");
}

/// The classes float upright with freeboard, sit level, carry reserve buoyancy, and are fitted to fight:
/// engines, screws, rudders, fuel, power, a bridge, berths and pumps; combatants a sensor and a weapon.
#[test]
fn every_class_floats_and_is_fitted() {
    let cat = cat();
    let rows = classes();
    for (spec, b) in ALL.iter().zip(built()) {
        let r = &b.report;
        let f = spec.file;
        assert!(r.gm_m >= gm_floor(spec) - 1e-9, "{f}: GM {:.2} m", r.gm_m);
        assert!(r.trim_deg.abs() <= TRIM_LIMIT_DEG + 1e-9, "{f}: trim {:.2} deg", r.trim_deg);
        assert!(r.freeboard_m >= 1.0, "{f}: freeboard {:.2} m", r.freeboard_m);
        assert!(r.displacement_at_deck_t >= 2.0 * r.stats.tonnage_t, "{f}: reserve buoyancy");
        assert!(r.power_kw >= 0.8 * r.load_kw, "{f}: {} kW for {} kW of load", r.power_kw, r.load_kw);
        let has = |p: &dyn Fn(&PartFunction) -> bool| b.blueprint.parts.iter().any(|x| cat.part(&x.part).is_some_and(|d| p(&d.function)));
        for (what, ok) in [
            ("engine", has(&|f| matches!(f, PartFunction::Engine(_)))),
            ("propulsor", has(&|f| matches!(f, PartFunction::Propulsor(_)))),
            ("rudder", has(&|f| matches!(f, PartFunction::Rudder { .. }))),
            ("fuel", has(&|f| matches!(f, PartFunction::FuelTank { .. }))),
            ("generator", has(&|f| matches!(f, PartFunction::Generator { .. }))),
            ("bridge", has(&|f| matches!(f, PartFunction::Console))),
            ("berths", has(&|f| matches!(f, PartFunction::Berths { .. }))),
            ("pumps", has(&|f| matches!(f, PartFunction::Pump { .. }))),
            ("sensor", has(&|f| matches!(f, PartFunction::Sensor(_)))),
        ] {
            assert!(ok, "{f} has no {what}");
        }
        let row = rows.iter().find(|c| c.blueprint == format!("combine/act1/{f}.bp.ron")).unwrap();
        let merchant = matches!(row.role, ClassRole::Freighter | ClassRole::Tanker | ClassRole::Civilian);
        let armed = has(&|f| matches!(f, PartFunction::Gun(_) | PartFunction::Launcher(_)));
        assert_eq!(armed, !merchant, "{f}: combat classes and tenders are armed, merchants are not");
        // A gun the ship cannot aim past a few kilometres is no warship gun: every mount over 30 mm has a director.
        let big_gun = has(&|f| matches!(f, PartFunction::Gun(g) if g.calibre_mm > 30 && !g.point_defence));
        if big_gun { assert!(has(&|f| matches!(f, PartFunction::Director)), "{f}: a medium gun without a director"); }
    }
}

#[test]
#[ignore = "writes blueprints/combine/act1; run by hand after editing the build sheets"]
fn write_combine_act1() {
    let cat = cat();
    let dir = out_dir();
    std::fs::create_dir_all(&dir).unwrap();
    let mut built = Vec::new();
    for spec in ALL {
        let b = build(spec, &cat);
        let issues = issues(&b.blueprint, &cat);
        if !issues.is_empty() { println!("{}: {:?}", spec.file, issues); }
        std::fs::write(dir.join(format!("{}.bp.ron", spec.file)), content::blueprint::io::to_file_text(&b.blueprint)).unwrap();
        built.push((spec.file, b));
    }
    println!("{}", table(&built));
    let errors: Vec<&String> = built.iter().flat_map(|(_, b)| &b.report.errors).collect();
    for e in &errors { println!("{e}"); }
    assert!(errors.is_empty(), "{} fits found no room", errors.len());
}

#[test]
#[ignore = "debug aid"]
fn print_sections() {
    let which = std::env::var("CLASS").unwrap_or_default();
    for spec in ALL.iter().filter(|s| which.is_empty() || s.file == which) {
        println!("{}", combine::builder::sections(spec, &[0.05, 0.3, 0.5, 0.7, 0.85]));
    }
}

#[test]
#[ignore = "debug aid: side elevations of the committed classes"]
fn print_profiles() {
    let cat = cat();
    let which = std::env::var("CLASS").unwrap_or_default();
    for spec in ALL.iter().filter(|s| which.is_empty() || s.file == which) {
        let text = std::fs::read_to_string(out_dir().join(format!("{}.bp.ron", spec.file))).unwrap();
        let bp = content::blueprint::io::load_blueprint_with(&text, spec.file, &cat).unwrap().blueprint;
        let g = content::blueprint::io::build_grid(&bp, &cat);
        let wl = bp.hull.design_waterline_y;
        println!("{} ({} x {} x {})", spec.name, g.size[0], g.size[1], g.size[2]);
        for y in (0..g.size[1] as i16).rev() {
            let row: String = (0..g.size[0] as i16).map(|x| {
                let mut ch = if y == wl { '~' } else { ' ' };
                for z in 0..g.size[2] as i16 {
                    let c = g.get(content::cell::CellPos::new(x, y, z));
                    if c.is_empty() { continue; }
                    ch = match c.material { content::cell::Material::PartBody => 'o', content::cell::Material::Glass => '=', _ => if ch == 'o' { 'o' } else { '#' } };
                    if ch == 'o' { break; }
                }
                ch
            }).collect();
            println!("{row}");
        }
    }
}

#[test]
#[ignore = "debug aid: draft of the stock starters below their design waterline"]
fn print_starter_drafts() {
    let cat = cat();
    for (p, t) in data::stock_blueprint_files().filter(|(p, _)| p.starts_with("starters/")) {
        let bp = content::blueprint::io::load_blueprint_with(t, p, &cat).unwrap().blueprint;
        let g = content::blueprint::io::build_grid(&bp, &cat);
        let low = (0..g.size[1] as i16).find(|&y| (0..g.size[0] as i16).any(|x| (0..g.size[2] as i16).any(|z| !g.get(content::cell::CellPos::new(x, y, z)).is_empty()))).unwrap();
        println!("{p}: waterline row {} lowest row {low} draft {:.1} m", bp.hull.design_waterline_y, (bp.hull.design_waterline_y - low) as f64 * 0.5);
    }
}

