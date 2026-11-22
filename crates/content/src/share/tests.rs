use super::*;
use crate::blueprint::io::{compute_hash, load_blueprint_with, to_file_text};
use crate::blueprint::Blueprint;
use crate::catalogue::Catalogue;
use crate::graph::ScriptSource;
use base64::Engine;

const BP: &str = include_str!("../../../../blueprints/examples/minimal.bp.ron");
const KIT: &str = include_str!("../../../../blueprints/examples/minimal.kit.ron");

fn ship() -> Blueprint { load_blueprint_with(BP, "bp", &Catalogue::default()).unwrap().blueprint }
fn kit() -> crate::kit::KitDef { crate::kit::parse_kit(KIT, "kit").unwrap() }
fn b64(bytes: &[u8]) -> String { base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(bytes) }

#[test]
fn codec_round_trips_within_bounds() {
    let data: Vec<u8> = (0..50_000u32).map(|i| (i % 251) as u8 ^ (i / 97) as u8).collect();
    let frame = compress(&data);
    assert!(frame.len() < data.len());
    assert_eq!(decompress_bounded(&frame, data.len()).unwrap(), data);
    assert!(matches!(decompress_bounded(&frame, data.len() - 1), Err(ContentError::Limit(_))));
    assert_eq!(decompress_bounded(&compress(b""), 0).unwrap(), b"");
    // Trailing bytes, truncation, a second frame and garbage are all refused.
    let mut trailing = frame.clone();
    trailing.push(0);
    assert!(decompress_bounded(&trailing, 1 << 20).is_err());
    assert!(decompress_bounded(&frame[..frame.len() - 3], 1 << 20).is_err());
    let mut two = frame.clone();
    two.extend_from_slice(&frame);
    assert!(decompress_bounded(&two, 1 << 20).is_err());
    assert!(decompress_bounded(b"not zstd at all", 1 << 20).is_err());
    assert!(decompress_bounded(&[], 1 << 20).is_err());
    // A flipped byte in the body trips the frame checksum or the block decoder.
    let mut flipped = frame.clone();
    let mid = flipped.len() / 2;
    flipped[mid] ^= 0x5A;
    assert!(decompress_bounded(&flipped, 1 << 20).is_err());
}

#[test]
fn frame_without_declared_size_is_still_bounded() {
    // Streamed frames carry no content size; the bound must hold while decoding.
    let data = vec![7u8; 300_000];
    let mut enc = zstd::stream::write::Encoder::new(Vec::new(), 3).unwrap();
    std::io::Write::write_all(&mut enc, &data).unwrap();
    let frame = enc.finish().unwrap();
    assert!(matches!(zstd::zstd_safe::get_frame_content_size(&frame), Ok(None)));
    assert!(matches!(decompress_bounded(&frame, 100_000), Err(ContentError::Limit(_))));
    assert_eq!(decompress_bounded(&frame, 300_000).unwrap(), data);
}

#[test]
fn ship_share_string_round_trips() {
    let bp = ship();
    let s = encode_share_string(&Shareable::Ship(Box::new(bp.clone())));
    assert!(s.starts_with(SHARE_PREFIX));
    assert!(looks_like_share_string(&format!("  {s}")));
    assert!(s[SHARE_PREFIX.len()..].bytes().all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_'));
    let back = decode_share_string_with(&s, &Catalogue::default()).unwrap();
    assert_eq!(back, Shareable::Ship(Box::new(bp.clone())));
    // Chat clients wrap long text; whitespace anywhere is ignored, and stray padding is tolerated.
    let wrapped: String = s.chars().enumerate().flat_map(|(i, c)| if i % 17 == 16 { vec![c, '\n', ' '] } else { vec![c] }).collect();
    assert_eq!(decode_share_string_with(&format!("\t{wrapped}==\r\n"), &Catalogue::default()).unwrap(), back);
}

#[test]
fn kit_share_string_round_trips() {
    let k = kit();
    let s = encode_share_string(&Shareable::Kit(Box::new(k.clone())));
    assert_eq!(decode_share_string_with(&s, &Catalogue::default()).unwrap(), Shareable::Kit(Box::new(k)));
}

#[test]
fn damaged_and_foreign_strings_are_refused() {
    let cat = Catalogue::default();
    assert!(matches!(decode_share_string_with("hello", &cat), Err(ContentError::NotAShareString)));
    assert!(matches!(decode_share_string_with("BPN1:***", &cat), Err(ContentError::NotAShareString)));
    assert!(matches!(decode_share_string_with("BPN2:abcd", &cat), Err(ContentError::FormatTooNew { found: 2, .. })));
    assert!(matches!(decode_share_string_with("BPN1:abcd", &cat), Err(ContentError::DamagedShareString)));
    // Content edited after hashing.
    let bp = ship();
    let tampered = to_file_text(&bp).replace("Minimal raft", "Maximal raft");
    let s = format!("{SHARE_PREFIX}{}", b64(&compress(tampered.as_bytes())));
    assert!(matches!(decode_share_string_with(&s, &cat), Err(ContentError::DamagedShareString)));
    // Content with no hash at all.
    let s = format!("{SHARE_PREFIX}{}", b64(&compress(BP.as_bytes())));
    assert!(matches!(decode_share_string_with(&s, &cat), Err(ContentError::DamagedShareString)));
    // A frame that expands past 16 MiB is refused before it is decoded.
    let huge = vec![b' '; crate::blueprint::MAX_RON_BYTES + 1];
    let frame = zstd::bulk::compress(&huge, 1).unwrap();
    let s = format!("{SHARE_PREFIX}{}", b64(&frame));
    assert!(matches!(decode_share_string_with(&s, &cat), Err(ContentError::Limit(_))));
    let long = "A".repeat(MAX_SHARE_CHARS + 1);
    assert!(matches!(decode_share_string_with(&long, &cat), Err(ContentError::Limit(_))));
}

#[test]
fn imported_scripts_arrive_disabled() {
    let mut bp = ship();
    bp.graph.scripts.push(ScriptSource { name: "s".into(), source: "return 1".into(), inputs: vec![], outputs: vec![], enabled: true });
    bp.header.hash = compute_hash(&bp);
    let s = encode_share_string(&Shareable::Ship(Box::new(bp.clone())));
    let Shareable::Ship(back) = decode_share_string_with(&s, &Catalogue::default()).unwrap() else { panic!("not a ship") };
    assert!(!back.graph.scripts[0].enabled);
    assert_eq!(back.header.hash, compute_hash(&back));
    // The network form keeps the owner's settings and its address.
    let packed = pack(&Shareable::Ship(Box::new(bp.clone())));
    let Shareable::Ship(net) = unpack_with(&packed, &Catalogue::default()).unwrap() else { panic!("not a ship") };
    assert!(net.graph.scripts[0].enabled);
    assert_eq!(net.header.hash, packed.hash);
}

#[test]
fn pack_round_trips_and_checks_its_address() {
    let cat = Catalogue::default();
    for item in [Shareable::Ship(Box::new(ship())), Shareable::Kit(Box::new(kit()))] {
        let packed = pack(&item);
        assert_eq!(unpack_with(&packed, &cat).unwrap(), item);
        let mut wrong = packed.clone();
        wrong.hash = ContentHash([9; 32]);
        assert!(matches!(unpack_with(&wrong, &cat), Err(ContentError::DamagedShareString)));
        let mut corrupt = packed.clone();
        corrupt.compressed.truncate(corrupt.compressed.len() / 2);
        assert!(matches!(unpack_with(&corrupt, &cat), Err(ContentError::DamagedShareString)));
    }
    let too_big = PackedBlueprint { hash: ContentHash::EMPTY, compressed: vec![0; crate::blueprint::MAX_COMPRESSED_BYTES + 1] };
    assert!(matches!(unpack_with(&too_big, &cat), Err(ContentError::Limit(_))));
    // The packed hash is the canonical item's hash, a fixed point of loading.
    let bp = ship();
    assert_eq!(pack(&Shareable::Ship(Box::new(bp.clone()))).hash, compute_hash(&bp));
}

#[test]
fn truncated_text_never_panics() {
    let cat = Catalogue::default();
    let mut bp = ship();
    bp.header.name = "Écume à l'étrave".into();
    let text = to_file_text(&bp);
    let kit_text = crate::kit::kit_to_file_text(&kit()).replace("76 mm gun", "Canon de 76 é");
    for t in [&text, &kit_text] {
        for end in (0..=t.len()).filter(|&i| t.is_char_boundary(i)) {
            let p = &t[..end];
            let _ = crate::kit::text::verify_text_hash(p);
            let _ = crate::kit::text::doc_kind(p);
            let _ = crate::kit::text::max_depth(p);
            let _ = load_blueprint_with(p, "p", &cat);
            let _ = crate::kit::parse_kit(p, "p");
        }
    }
    // An unterminated hash literal ending in a multi-byte character, through both entry points.
    let frame = compress("(hash: \"é".as_bytes());
    assert!(decode_share_string_with(&format!("{SHARE_PREFIX}{}", b64(&frame)), &cat).is_err());
    assert!(unpack_with(&PackedBlueprint { hash: ContentHash::EMPTY, compressed: frame }, &cat).is_err());
}

#[test]
fn unpack_refuses_content_that_loads_to_another_hash() {
    // A ship with a part this build does not know: loading removes it, so it is no longer the addressed design.
    let mut bp = ship();
    bp.parts.push(crate::blueprint::PartPlacement { part: crate::ids::PartId::new("part.from.a.mod"), pos: Default::default(), rot: Default::default(), kit: None, label: None, load: None });
    let packed = pack(&Shareable::Ship(Box::new(bp)));
    assert!(matches!(unpack_with(&packed, &Catalogue::default()), Err(ContentError::DamagedShareString)));
}
