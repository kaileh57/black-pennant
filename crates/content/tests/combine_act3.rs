//! Unit `data-act3`: the Combine's Act 3 classes are real blueprints, built by the editor's steps in
//! `combine/builder.rs` (shared with the Act 1 and Act 2 classes) from the build sheets in
//! `combine/classes_act3.rs`, and committed under `blueprints/combine/act3/`. Regenerate them with
//! `cargo test -p content --test combine_act3 -- --ignored write_combine_act3`.
mod combine {
    pub mod builder;
    pub mod classes_act3;
}
use combine::builder::{build, gm_floor, Built, ShipSpec, TRIM_LIMIT_DEG};
use combine::classes_act3::ALL;
use content::blueprint::BlueprintKind;
use content::campaign::{ClassRole, EnemyClassDef};
use content::catalogue::Catalogue;
use content::data::{self, parse_ron};
use content::enums::{Act, Tier};
use content::part::{LauncherKind, PartFunction};

fn cat() -> &'static Catalogue {
    static CAT: std::sync::OnceLock<Catalogue> = std::sync::OnceLock::new();
    CAT.get_or_init(|| Catalogue::load_embedded().expect("embedded catalogue loads"))
}

/// Validation in enemy-class mode under the open campaign ruleset with no tonnage cap.
fn issues(bp: &content::blueprint::Blueprint, cat: &Catalogue) -> Vec<content::validate::Issue> {
    let rs = content::ruleset::Ruleset { tonnage_cap_t: 0.0, ..content::ruleset::Ruleset::campaign(Act::Act3) };
    let ctx = content::validate::ValidationContext { ruleset: &rs, available: None, enemy_class: true, complexity: None };
    content::validate::validate_blueprint(bp, cat, &ctx).issues
}

fn out_dir() -> std::path::PathBuf { std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../blueprints/combine/act3") }

fn table(built: &[(&str, &Built)]) -> String {
    let mut s = String::from("class               t      light  cost  draft  dmax  deck_t  fb    GM     trim  deg    kW+    kW-   shaft  len   height\n");
    for (f, b) in built {
        let r = &b.report;
        s += &format!("{f:18} {:6.1} {:6.1} {:5} {:5.2} {:5.2} {:7.1} {:5.2} {:6.2} {:+6.3} {:+5.2} {:6.0} {:6.0} {:6.0} {:5.1} {:5.1}\n",
            r.stats.tonnage_t, r.stats.light_t, r.stats.cost, r.draft_m, r.draft_max_m, r.displacement_at_deck_t, r.freeboard_m, r.gm_m, r.trim_lever, r.trim_deg, r.power_kw, r.load_kw, r.shaft_kw, r.stats.length_m, full_height_m(&b.blueprint));
    }
    s
}

/// Build sheets in parallel, one thread per sheet; results come back in sheet order (the builder is
/// deterministic, so the order of completion never shows).
fn build_all(specs: &[&'static ShipSpec]) -> Vec<Built> {
    let cat = cat();
    std::thread::scope(|s| {
        let handles: Vec<_> = specs.iter().map(|spec| s.spawn(move || {
            let t0 = std::time::Instant::now();
            let b = build(spec, cat);
            if std::env::var_os("BUILD_TIMES").is_some() { println!("{}: built in {:.2} s", spec.file, t0.elapsed().as_secs_f64()); }
            b
        })).collect();
        handles.into_iter().map(|h| h.join().expect("a build sheet panicked")).collect()
    })
}

/// Every class built once per test process.
fn built() -> &'static [Built] {
    static BUILT: std::sync::OnceLock<Vec<Built>> = std::sync::OnceLock::new();
    BUILT.get_or_init(|| build_all(&ALL.iter().collect::<Vec<_>>()))
}

/// Keel to the top of the highest structure cell or part, metres (masts and sensors are parts).
fn full_height_m(bp: &content::blueprint::Blueprint) -> f64 {
    let rows = &bp.cells.0;
    let keel = rows.iter().map(|r| r.y as i32).min().unwrap_or(0);
    let mut top = rows.iter().map(|r| r.y as i32 + 1).max().unwrap_or(0);
    for p in &bp.parts {
        let Some(d) = cat().part(&p.part) else { continue };
        let s = content::kit::rotated_size(d.size, p.rot);
        top = top.max(p.pos.y as i32 + s[1] as i32);
    }
    (top - keel) as f64 * content::cell::CELL_M
}

fn classes() -> Vec<EnemyClassDef> { parse_ron(data::ENEMY_CLASSES[2], "enemy_classes/act3.ron").unwrap() }

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
    assert!(stale.is_empty(), "stale class blueprints {stale:?}: run `cargo test -p content --test combine_act3 -- --ignored write_combine_act3`");
}

/// Every class row names a committed blueprint that loads through the stock loader, validates in enemy-class
/// mode with no issue and carries the class row's name key. Only the capital row exceeds 90 m, and it stays within
/// the 150 m ceiling (L126); submarine rows are exactly the submarine-role classes.
#[test]
fn every_class_is_a_valid_stock_blueprint() {
    let cat = cat();
    let rows = classes();
    assert_eq!(rows.len(), ALL.len(), "one build sheet per class row");
    for row in &rows {
        assert_eq!(row.act, Act::Act3, "{}", row.id);
        let file = row.blueprint.strip_prefix("combine/act3/").and_then(|f| f.strip_suffix(".bp.ron")).unwrap_or_else(|| panic!("{}: path {}", row.id, row.blueprint));
        let spec = ALL.iter().find(|s| s.file == file).unwrap_or_else(|| panic!("{}: no build sheet {file}", row.id));
        let bp = content::campaign::load::stock_blueprint(&row.blueprint).unwrap_or_else(|e| panic!("{}: {e}", row.id));
        assert_eq!(bp.header.kind, BlueprintKind::EnemyClass, "{}", row.id);
        assert_eq!(bp.header.name_key.as_ref(), Some(&row.name), "{}", row.id);
        assert_eq!(row.name.as_str(), spec.key);
        let issues = issues(&bp, cat);
        assert!(issues.is_empty(), "{}: {issues:?}", row.id);
        let st = content::stats::compute_stats(&bp, cat);
        assert_eq!(row.capital, st.length_m > 90.0, "{} is {} m: capitals and only capitals exceed 90 m", row.id, st.length_m);
        assert!(st.length_m <= 150.0, "{}", row.id);
        assert_eq!(row.submersible, row.role == ClassRole::Submarine, "{}", row.id);
        if row.submersible { assert_eq!(row.behaviour, "submarine", "{}", row.id); }
        // A class declares the helicopter its deck carries, and only then.
        let decks: Vec<&content::ids::MunitionId> = bp.parts.iter().filter_map(|p| match cat.part(&p.part).map(|d| &d.function) { Some(PartFunction::HeliDeck { helicopter }) => Some(helicopter), _ => None }).collect();
        let wing: Vec<&content::ids::MunitionId> = row.air_wing.iter().map(|(m, _)| m).collect();
        assert_eq!(decks, wing, "{}: air wing matches its decks", row.id);
    }
    // About 14 classes (L873) across the five tiers, with the roles Act 3 operations need.
    for t in [Tier::T1, Tier::T2, Tier::T3, Tier::T4, Tier::T5] {
        assert!(rows.iter().any(|c| c.tier == t), "tier {t:?} has no class");
    }
    for role in [ClassRole::Freighter, ClassRole::Tanker, ClassRole::Civilian, ClassRole::Corvette, ClassRole::Frigate, ClassRole::Destroyer, ClassRole::Submarine] {
        assert!(rows.iter().any(|c| c.role == role), "no {role:?} class");
    }
    assert!((12..=16).contains(&rows.len()), "about 14 classes in Act 3");
    assert_eq!(rows.iter().filter(|r| r.capital).count(), 1, "one capital class in Act 3");
}

/// The classes float upright with freeboard, sit level, carry reserve buoyancy, and are fitted to fight. The
/// submarines also stay low enough to dive under the act's shallowest operating water.
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
        assert!(list_offset_m(b).abs() <= 0.015, "{f}: centre of mass {:+.3} m off the centreline", list_offset_m(b));
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
        let row = rows.iter().find(|c| c.blueprint == format!("combine/act3/{f}.bp.ron")).unwrap();
        let merchant = matches!(row.role, ClassRole::Freighter | ClassRole::Tanker | ClassRole::Civilian);
        let armed = has(&|f| matches!(f, PartFunction::Gun(_) | PartFunction::Launcher(_)));
        assert_eq!(armed, !merchant, "{f}: combat classes are armed, merchants are not");
        let big_gun = has(&|f| matches!(f, PartFunction::Gun(g) if g.calibre_mm > 30 && !g.point_defence));
        if big_gun { assert!(has(&|f| matches!(f, PartFunction::Director)), "{f}: a medium gun without a director"); }
        if row.submersible {
            // A boat runs on its batteries and electric motors and fights with torpedoes and sonar. Its full height
            // (keel to masthead) plus the AI's 3 m submergence margin and 6 m bottom clearance fits the 35 m water
            // of the shallowest Hollin maps, so it can always get under.
            assert!(b.blueprint.parts.iter().all(|x| !matches!(cat.part(&x.part).map(|d| &d.function), Some(PartFunction::Engine(e)) if e.kind != content::part::EngineKind::ElectricMotor)), "{f}: electric drive only");
            assert!(has(&|f| matches!(f, PartFunction::Battery { .. })), "{f}: batteries");
            assert!(has(&|f| matches!(f, PartFunction::Launcher(l) if l.kind == LauncherKind::TorpedoTube)), "{f}: torpedo tubes");
            assert!(has(&|f| matches!(f, PartFunction::Sensor(s) if s.kind == content::part::SensorKind::PassiveSonar)), "{f}: passive sonar");
            assert!(!has(&|f| matches!(f, PartFunction::Gun(_))), "{f}: no guns on a boat");
            let h = full_height_m(&b.blueprint);
            assert!(h + 3.0 + 6.0 <= 35.0, "{f}: {h} m tall");
            continue;
        }
        // Every surface combatant from tier 3 up can engage aircraft or the missiles they drop (Act 2's air threat
        // stays in service), and can hear a submarine.
        if !merchant && row.tier >= Tier::T3 {
            let pd = count(&|f| matches!(f, PartFunction::Gun(g) if g.point_defence) || matches!(f, PartFunction::Launcher(l) if l.kind == LauncherKind::Missile && matches!(l.munition.as_str(), "missile.sam" | "missile.pd")));
            assert!(pd >= 1, "{f}: no air defence");
            assert!(has(&|f| matches!(f, PartFunction::Sensor(s) if s.kind == content::part::SensorKind::PassiveSonar)), "{f}: no sonar");
        }
    }
}

/// Heavy guns arrive in Act 3 (L551): the gun frigates, destroyers and the cruiser carry them, each with its own
/// calibre's magazine and a hoist in the same kit.
#[test]
fn heavy_guns_sail_with_their_magazines() {
    let cat = cat();
    let mut heavy_classes = 0;
    for (spec, b) in ALL.iter().zip(built()) {
        let heavy: Vec<u16> = b.blueprint.parts.iter().filter_map(|x| match cat.part(&x.part).map(|d| &d.function) { Some(PartFunction::Gun(g)) if g.band == content::part::weapons::CalibreBand::Heavy => Some(g.calibre_mm), _ => None }).collect();
        if heavy.is_empty() { continue; }
        heavy_classes += 1;
        for cal in heavy {
            assert!(b.blueprint.parts.iter().any(|x| matches!(cat.part(&x.part).map(|d| &d.function), Some(PartFunction::Magazine { calibre_mm, .. }) if *calibre_mm == cal)), "{}: {cal} mm without a magazine", spec.file);
        }
    }
    assert!(heavy_classes >= 5, "{heavy_classes} classes carry heavy guns");
}

#[test]
#[ignore = "writes blueprints/combine/act3; run by hand after editing the build sheets"]
fn write_combine_act3() {
    let cat = cat();
    let dir = out_dir();
    std::fs::create_dir_all(&dir).unwrap();
    let which = std::env::var("CLASS").unwrap_or_default();
    let specs: Vec<&'static ShipSpec> = ALL.iter().filter(|s| which.is_empty() || s.file == which).collect();
    let t0 = std::time::Instant::now();
    let built = build_all(&specs);
    println!("built {} classes in {:.2} s", specs.len(), t0.elapsed().as_secs_f64());
    let mut rows = Vec::new();
    for (spec, b) in specs.iter().zip(&built) {
        let issues = issues(&b.blueprint, cat);
        for i in &issues {
            let slot = match i { content::validate::Issue::PartBadPlacement { slot } => Some(slot.0 as usize), _ => None };
            let at = slot.and_then(|k| b.blueprint.parts.get(k)).map(|p| format!(" ({} at {:?})", p.part, p.pos)).unwrap_or_default();
            println!("{}: {i:?}{at}", spec.file);
        }
        std::fs::write(dir.join(format!("{}.bp.ron", spec.file)), content::blueprint::io::to_file_text(&b.blueprint)).unwrap();
        rows.push((spec.file, b));
    }
    println!("{}", table(&rows));
    let errors: Vec<&String> = built.iter().flat_map(|b| &b.report.errors).collect();
    for e in &errors { println!("{e}"); }
    assert!(errors.is_empty(), "{} fits found no room", errors.len());
}

/// Transverse centre of mass off the centreline of the (symmetric) hull, metres, positive to starboard.
fn list_offset_m(b: &Built) -> f64 {
    let (z0, z1) = b.blueprint.cells.0.iter().fold((i16::MAX, i16::MIN), |(a, z), row| (a.min(row.z), z.max(row.z)));
    let centreline_m = (z0 as f64 + z1 as f64 + 1.0) * 0.5 * content::cell::CELL_M;
    b.report.stats.com_m[2] - centreline_m
}

/// Debug aid for trimming out a list: for each listing class, the change in transverse centre of mass that a
/// half-cell shift to port (`.dz(-1)`) of each centred deck or hold fit makes, heaviest moments first.
#[test]
#[ignore = "debug aid"]
fn print_list_trim_act3() {
    let which = std::env::var("CLASS").unwrap_or_default();
    for (spec, b) in ALL.iter().zip(built()).filter(|(s, _)| which.is_empty() || s.file == which) {
        let base = list_offset_m(b);
        println!("{}: centre of mass {base:+.4} m", spec.file);
        if base.abs() <= 0.015 && which.is_empty() { continue; }
        let candidates: Vec<usize> = spec.fits.iter().enumerate().filter(|(_, f)| f.dz == 0 && !matches!(f.at, combine::builder::At::Edge(..) | combine::builder::At::Stern(_))).map(|(i, _)| i).collect();
        let variants: Vec<&'static ShipSpec> = candidates.iter().map(|&i| {
            let mut fits = spec.fits.to_vec();
            fits[i] = fits[i].dz(-1);
            &*Box::leak(Box::new(ShipSpec { file: spec.file, name: spec.name, key: spec.key, hull: spec.hull, bulkheads: spec.bulkheads, platform_m: spec.platform_m, houses: spec.houses, fits: Box::leak(fits.into_boxed_slice()), armour: spec.armour, scheme: spec.scheme }))
        }).collect();
        let mut rows: Vec<(f64, usize)> = candidates.iter().zip(build_all(&variants)).filter(|(_, v)| v.report.errors.is_empty()).map(|(&i, v)| (list_offset_m(&v) - base, i)).collect();
        rows.sort_by(|a, b| a.0.partial_cmp(&b.0).unwrap());
        for (d, i) in rows { println!("  {d:+.4}  #{i} {:?}", spec.fits[i]); }
    }
}

#[test]
#[ignore = "debug aid"]
fn print_sections_act3() {
    let which = std::env::var("CLASS").unwrap_or_default();
    for spec in ALL.iter().filter(|s| which.is_empty() || s.file == which) {
        println!("{}", combine::builder::sections(spec, &[0.05, 0.3, 0.5, 0.7, 0.85]));
    }
}
