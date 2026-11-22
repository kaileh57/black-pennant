use super::*;
use std::net::{IpAddr, Ipv4Addr, Ipv6Addr};

fn secret() -> LobbySecret { LobbySecret([0x01, 0x23, 0x45, 0x67, 0x89, 0xAB, 0xCD, 0xEF, 0xFE, 0xDC]) }
fn v4() -> SocketAddr { SocketAddr::new(IpAddr::V4(Ipv4Addr::new(203, 0, 113, 7)), 47610) }

#[test]
fn crockford_round_trips_every_length() {
    for len in 0..40usize {
        let bytes: Vec<u8> = (0..len).map(|i| (i as u8).wrapping_mul(37).wrapping_add(11)).collect();
        let text = crockford_encode(&bytes);
        assert_eq!(text.len(), (len * 8).div_ceil(5));
        if len == 0 {
            assert_eq!(crockford_decode(&text), Err(JoinCodeError::WrongLength));
        } else {
            assert_eq!(crockford_decode(&text).unwrap(), bytes);
        }
    }
}

#[test]
fn crockford_matches_reference_vector() {
    // "f" = 0x66 = 01100 110(00) -> "CR"; "foobar" from the Crockford reference table.
    assert_eq!(crockford_encode(b"f"), "CR");
    assert_eq!(crockford_encode(b"foobar"), "CSQPYRK1E8");
    assert_eq!(crockford_decode("csqpyrk1e8").unwrap(), b"foobar");
}

#[test]
fn crockford_decode_is_forgiving_and_strict() {
    // Lower case, I/L as 1, O as 0, hyphens and spaces ignored.
    let a = crockford_decode("01-23 4567").unwrap();
    assert_eq!(crockford_decode("oI-23 4567").unwrap(), a);
    assert_eq!(crockford_decode("0l23-4567").unwrap(), a);
    assert_eq!(crockford_decode("ABCU"), Err(JoinCodeError::BadCharacter));
    assert_eq!(crockford_decode("AB*"), Err(JoinCodeError::BadCharacter));
    // Three characters cannot come from any byte count.
    assert_eq!(crockford_decode("ABC"), Err(JoinCodeError::WrongLength));
    // Non-zero padding bits are not canonical.
    assert_eq!(crockford_decode("CZ"), Err(JoinCodeError::BadCharacter));
    assert_eq!(crockford_decode(&"0".repeat(200)), Err(JoinCodeError::WrongLength));
}

#[test]
fn join_code_is_34_characters_for_ipv4() {
    let code = JoinCode::new(v4(), secret());
    let text = code.encode();
    assert_eq!(text.chars().filter(|c| *c != '-').count(), 34);
    assert!(text.split('-').all(|g| g.len() <= 4));
    assert_eq!(text.split('-').count(), 9);
    assert_eq!(JoinCode::decode(&text).unwrap(), code);
    assert_eq!(JoinCode::decode(&text.to_lowercase().replace('-', " ")).unwrap(), code);
}

#[test]
fn join_code_round_trips_ipv6_and_canonicalises_mapped() {
    let addr = SocketAddr::new(IpAddr::V6(Ipv6Addr::new(0x2001, 0xdb8, 1, 2, 3, 4, 5, 6)), 5000);
    let code = JoinCode::new(addr, secret());
    let text = code.encode();
    assert_eq!(text.chars().filter(|c| *c != '-').count(), 53);
    assert_eq!(JoinCode::decode(&text).unwrap(), code);

    let mapped = SocketAddr::new(IpAddr::V6(Ipv4Addr::new(203, 0, 113, 7).to_ipv6_mapped()), 47610);
    let decoded = JoinCode::decode(&JoinCode::new(mapped, secret()).encode()).unwrap();
    assert_eq!(decoded.addr, v4());
}

#[test]
fn join_code_layout_is_frozen() {
    let b = JoinCode::new(v4(), secret()).to_bytes();
    assert_eq!(b.len(), 21);
    assert_eq!(b[0], 1);
    assert_eq!(&b[1..3], &(crate::PROTOCOL_VERSION as u16).to_be_bytes());
    assert_eq!(&b[3..7], &[203, 0, 113, 7]);
    assert_eq!(&b[7..9], &47610u16.to_be_bytes());
    assert_eq!(&b[9..19], &secret().0);
    assert_eq!(&b[19..21], &blake3::hash(&b[..19]).as_bytes()[..2]);
}

#[test]
fn join_code_rejects_typos_versions_and_bad_addresses() {
    let text = JoinCode::new(v4(), secret()).encode();
    // Change one character: the checksum catches it.
    let mut chars: Vec<char> = text.chars().collect();
    chars[5] = if chars[5] == 'A' { 'B' } else { 'A' };
    let typo: String = chars.into_iter().collect();
    assert_eq!(JoinCode::decode(&typo), Err(JoinCodeError::Checksum));
    assert_eq!(JoinCode::decode(&text[..text.len() - 5]), Err(JoinCodeError::WrongLength));

    let other = JoinCode { protocol: crate::PROTOCOL_VERSION + 1, addr: v4(), secret: secret() };
    assert_eq!(
        JoinCode::decode(&other.encode()),
        Err(JoinCodeError::ProtocolMismatch { code: crate::PROTOCOL_VERSION + 1, ours: crate::PROTOCOL_VERSION })
    );

    let mut b = JoinCode::new(v4(), secret()).to_bytes();
    b[0] = 2;
    let n = b.len() - 2;
    let sum = checksum(&b[..n]);
    b[n..].copy_from_slice(&sum);
    assert_eq!(JoinCode::decode(&crockford_encode(&b)), Err(JoinCodeError::UnknownFormat(2)));

    let zero_port = JoinCode::new(SocketAddr::new(v4().ip(), 0), secret());
    assert_eq!(JoinCode::decode(&zero_port.encode()), Err(JoinCodeError::BadAddress));
}

#[test]
fn lobby_key_is_16_characters_and_round_trips() {
    let key = secret().lobby_key();
    assert_eq!(key.len(), LOBBY_KEY_CHARS);
    assert_eq!(LobbySecret::from_lobby_key(&key).unwrap(), secret());
    assert_eq!(LobbySecret::from_lobby_key(&key[..8]), Err(JoinCodeError::WrongLength));
    assert_eq!(LobbySecret::from_lobby_key(&format!("{} {}", &key[..8], &key[8..])).unwrap(), secret());
}

#[test]
fn join_input_takes_a_code_or_an_address_and_key() {
    let code = JoinCode::new(v4(), secret());
    assert_eq!(parse_join_input(&format!("  {}  ", code.encode())).unwrap(), JoinTarget::Code(code));

    let key = secret().lobby_key();
    let t = parse_join_input(&format!("203.0.113.7:47610 {key}")).unwrap();
    assert_eq!(t, JoinTarget::Address { addr: v4(), secret: secret() });
    let t = parse_join_input(&format!("[2001:db8::1]:9000 {} {}", &key[..8], &key[8..])).unwrap();
    assert_eq!(t.addr(), "[2001:db8::1]:9000".parse().unwrap());
    // A bare address takes the default port.
    assert_eq!(parse_join_input(&format!("192.168.1.20 {key}")).unwrap().addr().port(), crate::DEFAULT_PORT);

    assert_eq!(parse_join_input(""), Err(JoinCodeError::WrongLength));
    assert_eq!(parse_join_input("203.0.113.7:47610"), Err(JoinCodeError::WrongLength));
    assert_eq!(parse_join_input(&format!("0.0.0.0:47610 {key}")), Err(JoinCodeError::BadAddress));
    assert_eq!(parse_join_input("ship ahoy!"), Err(JoinCodeError::BadCharacter));
}

#[test]
fn netcode_key_is_the_frozen_derive_key() {
    let k = secret().netcode_key();
    assert_eq!(k, blake3::derive_key("Black Pennant join key v1", &secret().0));
    assert_ne!(k, LobbySecret([0; SECRET_BYTES]).netcode_key());
}

#[test]
fn generated_values_are_fresh() {
    assert_ne!(LobbySecret::generate(), LobbySecret::generate());
    let id = ClientIdentity::generate("  Captain with a very long name indeed  ");
    assert!(id.client_id != 0 && id.client_id != u64::MAX);
    assert_eq!(id.name.chars().count(), crate::lobby::MAX_NAME_CHARS);
    assert!(id.name.starts_with("Captain"));
}

#[test]
fn connect_token_lists_server_then_sentinel() {
    let t = mint_connect_token(v4(), &secret(), 42, std::time::Duration::from_secs(1_800_000_000)).unwrap();
    assert_eq!(t.client_id, 42);
    assert_eq!(t.protocol_id, crate::NETCODE_PROTOCOL_ID);
    assert_eq!(t.server_addresses[0], Some(v4()));
    assert_eq!(t.server_addresses[1], Some(crate::transport::netcode::TOKEN_SENTINEL_ADDR));
    assert_eq!(t.server_addresses[2], None);
    assert_eq!(t.expire_timestamp - t.create_timestamp, crate::transport::netcode::TOKEN_EXPIRE_S);
}
