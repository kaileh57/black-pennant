//! Unit `data-act2`: the Combine's Act 2 classes are real blueprints, built by the editor's steps in
//! `combine/builder.rs` (shared with the Act 1 classes) from the build sheets in `combine/classes_act2.rs`, and
//! committed under `blueprints/combine/act2/`. Regenerate them with
//! `cargo test -p content --test combine_act2 -- --ignored write_combine_act2`.
mod combine {
    pub mod builder;
    pub mod classes_act2;
}
use combine::builder::{build, gm_floor, Built, TRIM_LIMIT_DEG};
use combine::classes_act2::ALL;
use content::blueprint::BlueprintKind;
use content::campaign::{ClassRole, EnemyClassDef};
use content::catalogue::Catalogue;
use content::data::{self, parse_ron};
use content::enums::{Act, Tier};
use content::part::PartFunction;

fn cat() -> Catalogue { Catalogue::load_embedded().expect("embedded catalogue loads") }

/// Validation in enemy-class mode under the open campaign ruleset with no tonnage cap.
fn issues(bp: &content::blueprint::Blueprint, cat: &Catalogue) -> Vec<content::validate::Issue> {
    let rs = content::ruleset::Ruleset { tonnage_cap_t: 0.0, ..content::ruleset::Ruleset::campaign(Act::Act2) };
    let ctx = content::validate::ValidationContext { ruleset: &rs, available: None, enemy_class: true, complexity: None };
    content::validate::validate_blueprint(bp, cat, &ctx).issues
}

fn out_dir() -> std::path::PathBuf { std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../blueprints/combine/act2") }

fn table(built: &[(&str, Built)]) -> String {
    let mut s = String::from("class               t      light  cost  draft  dmax  deck_t  fb    GM     trim  deg    kW+    kW-   shaft  len\n");
    for (f, b) in built {
        let r = &b.report;
        s += &format!("{f:18} {:6.1} {:6.1} {:5} {:5.2} {:5.2} {:7.1} {:5.2} {:6.2} {:+6.3} {:+5.2} {:6.0} {:6.0} {:6.0} {:5.1}\n",
            r.stats.tonnage_t, r.stats.light_t, r.stats.cost, r.draft_m, r.draft_max_m, r.displacement_at_deck_t, r.freeboard_m, r.gm_m, r.trim_lever, r.trim_deg, r.power_kw, r.load_kw, r.shaft_kw, r.stats.length_m);
    }
    s
}

/// Every class built once per test process (the build sheets are deterministic).
fn built() -> &'static [Built] {
    static BUILT: std::sync::OnceLock<Vec<Built>> = std::sync::OnceLock::new();
    BUILT.get_or_init(|| { let cat = cat(); ALL.iter().map(|s| build(s, &cat)).collect() })
}

fn classes() -> Vec<EnemyClassDef> { parse_ron(data::ENEMY_CLASSES[1], "enemy_classes/act2.ron").unwrap() }

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
    assert!(stale.is_empty(), "stale class blueprints {stale:?}: run `cargo test -p content --test combine_act2 -- --ignored write_combine_act2`");
}

/// Every class row names a committed blueprint that loads through the stock loader, validates in enemy-class
/// mode with no issue, carries the class row's name key and stays under the 90 m capital line.
#[test]
fn every_class_is_a_valid_stock_blueprint() {
    let cat = cat();
    let rows = classes();
    assert_eq!(rows.len(), ALL.len(), "one build sheet per class row");
    for row in &rows {
        assert_eq!(row.act, Act::Act2, "{}", row.id);
        let file = row.blueprint.strip_prefix("combine/act2/").and_then(|f| f.strip_suffix(".bp.ron")).unwrap_or_else(|| panic!("{}: path {}", row.id, row.blueprint));
        let spec = ALL.iter().find(|s| s.file == file).unwrap_or_else(|| panic!("{}: no build sheet {file}", row.id));
        let bp = content::campaign::load::stock_blueprint(&row.blueprint).unwrap_or_else(|e| panic!("{}: {e}", row.id));
        assert_eq!(bp.header.kind, BlueprintKind::EnemyClass, "{}", row.id);
        assert_eq!(bp.header.name_key.as_ref(), Some(&row.name), "{}", row.id);
        assert_eq!(row.name.as_str(), spec.key);
        let issues = issues(&bp, &cat);
        assert!(issues.is_empty(), "{}: {issues:?}", row.id);
        let st = content::stats::compute_stats(&bp, &cat);
        assert!(st.length_m <= 90.0, "{} is {} m; capitals over 90 m must say so", row.id, st.length_m);
        assert!(!row.capital && !row.submersible, "{}", row.id);
        // A class declares the helicopter its deck carries, and only then.
        let decks: Vec<&content::ids::MunitionId> = bp.parts.iter().filter_map(|p| match cat.part(&p.part).map(|d| &d.function) { Some(PartFunction::HeliDeck { helicopter }) => Some(helicopter), _ => None }).collect();
        let wing: Vec<&content::ids::MunitionId> = row.air_wing.iter().map(|(m, _)| m).collect();
        assert_eq!(decks, wing, "{}: air wing matches its decks", row.id);
    }
    // About 12 classes (L870) across the five tiers, with the roles Act 2 operations need.
    for t in [Tier::T1, Tier::T2, Tier::T3, Tier::T4, Tier::T5] {
        assert!(rows.iter().any(|c| c.tier == t), "tier {t:?} has no class");
    }
    for role in [ClassRole::Freighter, ClassRole::Tanker, ClassRole::Civilian, ClassRole::Picket, ClassRole::Corvette, ClassRole::Frigate] {
        assert!(rows.iter().any(|c| c.role == role), "no {role:?} class");
    }
    assert!((10..=14).contains(&rows.len()), "about 12 classes in Act 2");
}

/// The classes float upright with freeboard, sit level, carry reserve buoyancy, and are fitted to fight.
#[test]
fn every_class_floats_and_is_fitted() {
    let cat = cat();
    let rows = classes();
    for (spec, b) in ALL.iter().zip(built()) {
        let r = &b.report;
        let f = spec.file;
        assert!(r.gm_m >= gm_floor(spec) - 1e-9, "{f}: GM {:.2} m", r.gm_m);
        assert!(r.trim_deg.abs() <= TRIM_LIMIT_DEG + 1e-9, "{f}: trim {:.2} deg", r.trim_deg);
        // No list: the transverse centre of mass sits on the centreline of the (symmetric) hull to within 1.5 cm.
        let (z0, z1) = b.blueprint.cells.0.iter().fold((i16::MAX, i16::MIN), |(a, z), row| (a.min(row.z), z.max(row.z)));
        let centreline_m = (z0 as f64 + z1 as f64 + 1.0) * 0.5 * content::cell::CELL_M;
        assert!((r.stats.com_m[2] - centreline_m).abs() <= 0.015, "{f}: centre of mass {:+.3} m off the centreline", r.stats.com_m[2] - centreline_m);
        assert!(r.freeboard_m >= 1.0, "{f}: freeboard {:.2} m", r.freeboard_m);
        assert!(r.displacement_at_deck_t >= 2.0 * r.stats.tonnage_t, "{f}: reserve buoyancy");
        assert!(r.power_kw >= 0.8 * r.load_kw, "{f}: {} kW for {} kW of load", r.power_kw, r.load_kw);
        let has = |p: &dyn Fn(&PartFunction) -> bool| b.blueprint.parts.iter().any(|x| cat.part(&x.part).is_some_and(|d| p(&d.function)));
        let count = |p: &dyn Fn(&PartFunction) -> bool| b.blueprint.parts.iter().filter(|x| cat.part(&x.part).is_some_and(|d| p(&d.function))).count();
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
        // Each screw takes at most its rated shaft power: the engines never outrun the propellers.
        let shaft: f64 = b.blueprint.parts.iter().filter_map(|x| match cat.part(&x.part).map(|d| &d.function) { Some(PartFunction::Engine(e)) => Some(e.shaft_kw), _ => None }).sum();
        let absorb: f64 = b.blueprint.parts.iter().filter_map(|x| match cat.part(&x.part).map(|d| &d.function) { Some(PartFunction::Propulsor(p)) => Some(p.max_shaft_kw), _ => None }).sum();
        assert!(shaft <= absorb + 1e-6, "{f}: {shaft} kW of engines on {absorb} kW of screws");
        let row = rows.iter().find(|c| c.blueprint == format!("combine/act2/{f}.bp.ron")).unwrap();
        let merchant = matches!(row.role, ClassRole::Freighter | ClassRole::Tanker | ClassRole::Civilian);
        let armed = has(&|f| matches!(f, PartFunction::Gun(_) | PartFunction::Launcher(_)));
        assert_eq!(armed, !merchant, "{f}: combat classes are armed, merchants are not");
        let big_gun = has(&|f| matches!(f, PartFunction::Gun(g) if g.calibre_mm > 30 && !g.point_defence));
        if big_gun { assert!(has(&|f| matches!(f, PartFunction::Director)), "{f}: a medium gun without a director"); }
        // Act 2 brings air attack: every combatant from tier 3 up can engage aircraft or the missiles they drop.
        if !merchant && row.tier >= Tier::T3 {
            let pd = count(&|f| matches!(f, PartFunction::Gun(g) if g.point_defence) || matches!(f, PartFunction::Launcher(l) if l.kind == content::part::LauncherKind::Missile && matches!(l.munition.as_str(), "missile.sam" | "missile.pd")));
            assert!(pd >= 1, "{f}: no air defence");
        }
    }
}

#[test]
#[ignore = "writes blueprints/combine/act2; run by hand after editing the build sheets"]
fn write_combine_act2() {
    let cat = cat();
    let dir = out_dir();
    std::fs::create_dir_all(&dir).unwrap();
    let mut built = Vec::new();
    let which = std::env::var("CLASS").unwrap_or_default();
    for spec in ALL.iter().filter(|s| which.is_empty() || s.file == which) {
        let t0 = std::time::Instant::now();
        let b = build(spec, &cat);
        println!("{}: built in {:.2} s", spec.file, t0.elapsed().as_secs_f64());
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
fn print_sections_act2() {
    let which = std::env::var("CLASS").unwrap_or_default();
    for spec in ALL.iter().filter(|s| which.is_empty() || s.file == which) {
        println!("{}", combine::builder::sections(spec, &[0.05, 0.3, 0.5, 0.7, 0.85]));
    }
}
