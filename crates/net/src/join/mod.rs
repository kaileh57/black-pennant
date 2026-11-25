//! Unit `net-join`. Lobby secret, join codes, lobby keys, the netcode key, connect tokens and the install's
//! client identity. DESIGN.md L1001-1003.
//!
//! Join code: Crockford base32 (alphabet `0123456789ABCDEFGHJKMNPQRSTVWXYZ`; decoding is case-insensitive,
//! maps I and L to 1 and O to 0, and ignores hyphens and spaces), printed in groups of 4 joined by `-`.
//! Byte layout, frozen: format byte (bits 0..6 = version 1, bit 7 set = IPv6 address), protocol version as
//! u16 big-endian, address (4 or 16 bytes), port u16 big-endian, the 10-byte lobby secret, then the first 2
//! bytes of BLAKE3 over everything before it. 21 bytes for IPv4 = 34 characters (L1001 "about 34").
//! Lobby key: the same 10-byte secret as 16 Crockford characters, shown with the address for joining by
//! address. Netcode private key: `blake3::derive_key(JOIN_KEY_CONTEXT, secret)`. The joining client mints its
//! own connect token with that key, so traffic is encrypted and the secret is the lobby password;
//! regenerating it locks out old codes.
use crate::error::{JoinCodeError, NetError};
use serde::{Deserialize, Serialize};
use std::net::SocketAddr;

/// BLAKE3 `derive_key` context string. Frozen.
pub const JOIN_KEY_CONTEXT: &str = "Black Pennant join key v1";
pub const JOIN_CODE_VERSION: u8 = 1;
pub const JOIN_CODE_IPV6_FLAG: u8 = 0x80;
pub const SECRET_BYTES: usize = 10;
pub const LOBBY_KEY_CHARS: usize = 16;

/// The 10-byte per-session lobby secret (L1001).
#[derive(Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct LobbySecret(pub [u8; SECRET_BYTES]);
impl std::fmt::Debug for LobbySecret { fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result { f.write_str("LobbySecret(..)") } }

impl LobbySecret {
    /// Fresh secret from the OS RNG (`renet2_netcode::generate_random_bytes`). Unit: `net-join`.
    pub fn generate() -> LobbySecret { LobbySecret(renet2_netcode::generate_random_bytes::<SECRET_BYTES>()) }
    /// The 16-character lobby key. Unit: `net-join`.
    pub fn lobby_key(&self) -> String { crockford_encode(&self.0) }
    /// Parse a typed lobby key. Unit: `net-join`.
    pub fn from_lobby_key(text: &str) -> Result<LobbySecret, JoinCodeError> {
        let bytes = crockford_decode(text)?;
        let secret: [u8; SECRET_BYTES] = bytes.as_slice().try_into().map_err(|_| JoinCodeError::WrongLength)?;
        Ok(LobbySecret(secret))
    }
    /// The netcode private key. Unit: `net-join`.
    pub fn netcode_key(&self) -> [u8; 32] { blake3::derive_key(JOIN_KEY_CONTEXT, &self.0) }
}

/// A decoded join code.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct JoinCode { pub protocol: u32, pub addr: SocketAddr, pub secret: LobbySecret }

impl JoinCode {
    pub fn new(addr: SocketAddr, secret: LobbySecret) -> Self { Self { protocol: crate::PROTOCOL_VERSION, addr, secret } }
    /// Grouped Crockford text. Unit: `net-join`.
    pub fn encode(&self) -> String { group4(&crockford_encode(&self.to_bytes())) }
    /// Parse and verify the checksum. A code naming another protocol version decodes to
    /// `JoinCodeError::ProtocolMismatch` so the client refuses before connecting, naming both. Unit: `net-join`.
    pub fn decode(text: &str) -> Result<JoinCode, JoinCodeError> { JoinCode::from_bytes(&crockford_decode(text)?) }
}

/// What the player typed in the join field: a join code, or `address:port` plus a lobby key.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum JoinTarget { Code(JoinCode), Address { addr: SocketAddr, secret: LobbySecret } }

/// Parse join input: a join code, or an address and a lobby key separated by whitespace. Unit: `net-join`.
pub fn parse_join_input(text: &str) -> Result<JoinTarget, JoinCodeError> {
    let text = text.trim();
    let mut parts = text.split_whitespace();
    let first = parts.next().ok_or(JoinCodeError::WrongLength)?;
    if let Some(addr) = parse_addr(first) {
        // Address then lobby key; the key may itself be typed in space-separated groups.
        let key: String = parts.collect();
        if key.is_empty() {
            return Err(JoinCodeError::WrongLength);
        }
        check_addr(addr)?;
        return Ok(JoinTarget::Address { addr, secret: LobbySecret::from_lobby_key(&key)? });
    }
    JoinCode::decode(text).map(JoinTarget::Code)
}

impl JoinTarget {
    pub fn addr(&self) -> SocketAddr { match self { JoinTarget::Code(c) => c.addr, JoinTarget::Address { addr, .. } => *addr } }
    pub fn secret(&self) -> LobbySecret { match self { JoinTarget::Code(c) => c.secret, JoinTarget::Address { secret, .. } => *secret } }
}

/// Crockford base32 without padding. Unit: `net-join`.
pub fn crockford_encode(bytes: &[u8]) -> String {
    let mut out = String::with_capacity((bytes.len() * 8).div_ceil(5));
    let mut acc: u32 = 0;
    let mut bits: u32 = 0;
    for &b in bytes {
        acc = (acc << 8) | u32::from(b);
        bits += 8;
        while bits >= 5 {
            bits -= 5;
            out.push(ALPHABET[((acc >> bits) & 31) as usize] as char);
        }
        acc &= (1 << bits) - 1;
    }
    if bits > 0 {
        out.push(ALPHABET[((acc << (5 - bits)) & 31) as usize] as char);
    }
    out
}
/// Unit: `net-join`.
pub fn crockford_decode(text: &str) -> Result<Vec<u8>, JoinCodeError> {
    let mut out = Vec::with_capacity(text.len() * 5 / 8);
    let mut acc: u32 = 0;
    let mut bits: u32 = 0;
    let mut chars = 0usize;
    for c in text.chars() {
        if c == '-' || c.is_whitespace() {
            continue;
        }
        let v = crockford_value(c).ok_or(JoinCodeError::BadCharacter)?;
        chars += 1;
        if chars > MAX_DECODE_CHARS {
            return Err(JoinCodeError::WrongLength);
        }
        acc = (acc << 5) | u32::from(v);
        bits += 5;
        if bits >= 8 {
            bits -= 8;
            out.push((acc >> bits) as u8);
        }
        acc &= (1 << bits) - 1;
    }
    // Canonical lengths only: the trailing partial character carries fewer than 5 bits, all zero.
    if chars == 0 || chars != (out.len() * 8).div_ceil(5) {
        return Err(JoinCodeError::WrongLength);
    }
    if acc != 0 {
        return Err(JoinCodeError::BadCharacter);
    }
    Ok(out)
}

/// Mint a connect token for `server` (listed first, then `transport::netcode::TOKEN_SENTINEL_ADDR`) with the
/// secret's netcode key. Unit: `net-join`.
pub(crate) fn mint_connect_token(server: SocketAddr, secret: &LobbySecret, client_id: u64, unix_now: std::time::Duration) -> Result<renet2_netcode::ConnectToken, NetError> {
    use crate::transport::netcode::{NETCODE_TIMEOUT_S, TOKEN_EXPIRE_S, TOKEN_SENTINEL_ADDR};
    let mut addresses = vec![server];
    if server != TOKEN_SENTINEL_ADDR {
        addresses.push(TOKEN_SENTINEL_ADDR);
    }
    renet2_netcode::ConnectToken::generate(
        unix_now,
        crate::NETCODE_PROTOCOL_ID,
        TOKEN_EXPIRE_S,
        client_id,
        NETCODE_TIMEOUT_S,
        0,
        addresses,
        None,
        &secret.netcode_key(),
    )
    .map_err(|e| NetError::Transport(format!("connect token: {e}")))
}

/// The install's identity, stored as a small RON file in the user data directory by gdclient (L1003).
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ClientIdentity { pub client_id: u64, pub name: String }
impl ClientIdentity {
    /// New random id (never 0 or `u64::MAX`), default name. Unit: `net-join`.
    pub fn generate(default_name: &str) -> ClientIdentity {
        let client_id = loop {
            let id = u64::from_le_bytes(renet2_netcode::generate_random_bytes::<8>());
            if id != 0 && id != u64::MAX {
                break id;
            }
        };
        let name: String = default_name.trim().chars().take(crate::lobby::MAX_NAME_CHARS).collect();
        ClientIdentity { client_id, name: name.trim_end().to_string() }
    }
}

/// Crockford alphabet, value order.
const ALPHABET: &[u8; 32] = b"0123456789ABCDEFGHJKMNPQRSTVWXYZ";
/// No valid input is longer than an IPv6 join code (33 bytes = 53 characters); a cap keeps hostile input cheap.
const MAX_DECODE_CHARS: usize = 64;
/// Bytes before the checksum and the checksum length.
const IPV4_CODE_BYTES: usize = 1 + 2 + 4 + 2 + SECRET_BYTES + CHECKSUM_BYTES;
const IPV6_CODE_BYTES: usize = 1 + 2 + 16 + 2 + SECRET_BYTES + CHECKSUM_BYTES;
const CHECKSUM_BYTES: usize = 2;

fn crockford_value(c: char) -> Option<u8> {
    let c = c.to_ascii_uppercase();
    Some(match c {
        'O' => 0,
        'I' | 'L' => 1,
        '0'..='9' => c as u8 - b'0',
        _ => ALPHABET.iter().position(|&a| a == c as u8)? as u8,
    })
}

/// Groups of 4 joined by `-`.
fn group4(text: &str) -> String {
    let mut out = String::with_capacity(text.len() + text.len() / 4);
    for (i, c) in text.chars().enumerate() {
        if i > 0 && i % 4 == 0 {
            out.push('-');
        }
        out.push(c);
    }
    out
}

fn checksum(body: &[u8]) -> [u8; CHECKSUM_BYTES] {
    let h = blake3::hash(body);
    [h.as_bytes()[0], h.as_bytes()[1]]
}

/// `address:port`, `[v6]:port`, or a bare address (default port).
fn parse_addr(text: &str) -> Option<SocketAddr> {
    if let Ok(a) = text.parse::<SocketAddr>() {
        return Some(a);
    }
    let bare = text.strip_prefix('[').and_then(|t| t.strip_suffix(']')).unwrap_or(text);
    bare.parse::<std::net::IpAddr>().ok().map(|ip| SocketAddr::new(ip, crate::DEFAULT_PORT))
}

/// A join target must name a reachable unicast address and a real port.
fn check_addr(addr: SocketAddr) -> Result<(), JoinCodeError> {
    let ip = addr.ip();
    let bad_v4 = matches!(ip, std::net::IpAddr::V4(v4) if v4.is_broadcast());
    if addr.port() == 0 || ip.is_unspecified() || ip.is_multicast() || bad_v4 {
        return Err(JoinCodeError::BadAddress);
    }
    Ok(())
}

/// IPv4-mapped IPv6 addresses travel as IPv4, which keeps the code short.
fn canonical(addr: SocketAddr) -> SocketAddr {
    match addr {
        SocketAddr::V6(v6) => match v6.ip().to_ipv4_mapped() {
            Some(v4) => SocketAddr::new(std::net::IpAddr::V4(v4), v6.port()),
            None => addr,
        },
        SocketAddr::V4(_) => addr,
    }
}

impl JoinCode {
    /// The frozen byte layout (see the module header).
    fn to_bytes(self) -> Vec<u8> {
        let addr = canonical(self.addr);
        let mut b = Vec::with_capacity(IPV6_CODE_BYTES);
        let v6 = addr.is_ipv6();
        b.push(JOIN_CODE_VERSION | if v6 { JOIN_CODE_IPV6_FLAG } else { 0 });
        // The layout holds the protocol as u16; versions past 65,535 saturate (and then mismatch loudly).
        b.extend_from_slice(&(self.protocol.min(u32::from(u16::MAX)) as u16).to_be_bytes());
        match addr.ip() {
            std::net::IpAddr::V4(ip) => b.extend_from_slice(&ip.octets()),
            std::net::IpAddr::V6(ip) => b.extend_from_slice(&ip.octets()),
        }
        b.extend_from_slice(&addr.port().to_be_bytes());
        b.extend_from_slice(&self.secret.0);
        let sum = checksum(&b);
        b.extend_from_slice(&sum);
        b
    }

    fn from_bytes(b: &[u8]) -> Result<JoinCode, JoinCodeError> {
        let format = *b.first().ok_or(JoinCodeError::WrongLength)?;
        let version = format & !JOIN_CODE_IPV6_FLAG;
        if version != JOIN_CODE_VERSION {
            return Err(JoinCodeError::UnknownFormat(version));
        }
        let v6 = format & JOIN_CODE_IPV6_FLAG != 0;
        if b.len() != if v6 { IPV6_CODE_BYTES } else { IPV4_CODE_BYTES } {
            return Err(JoinCodeError::WrongLength);
        }
        let (body, sum) = b.split_at(b.len() - CHECKSUM_BYTES);
        if checksum(body) != sum {
            return Err(JoinCodeError::Checksum);
        }
        let protocol = u32::from(u16::from_be_bytes([body[1], body[2]]));
        let ip_len = if v6 { 16 } else { 4 };
        let ip_bytes = &body[3..3 + ip_len];
        let ip = if v6 {
            let o: [u8; 16] = ip_bytes.try_into().map_err(|_| JoinCodeError::WrongLength)?;
            std::net::IpAddr::V6(std::net::Ipv6Addr::from(o))
        } else {
            let o: [u8; 4] = ip_bytes.try_into().map_err(|_| JoinCodeError::WrongLength)?;
            std::net::IpAddr::V4(std::net::Ipv4Addr::from(o))
        };
        let p = 3 + ip_len;
        let port = u16::from_be_bytes([body[p], body[p + 1]]);
        let secret: [u8; SECRET_BYTES] = body[p + 2..p + 2 + SECRET_BYTES].try_into().map_err(|_| JoinCodeError::WrongLength)?;
        let addr = SocketAddr::new(ip, port);
        check_addr(addr)?;
        let ours = crate::PROTOCOL_VERSION;
        if protocol != ours {
            return Err(JoinCodeError::ProtocolMismatch { code: protocol, ours });
        }
        Ok(JoinCode { protocol, addr, secret: LobbySecret(secret) })
    }
}

#[cfg(test)]
mod tests;
