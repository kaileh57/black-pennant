//! Unit `net-schema`. Bit packing for snapshot frames. DESIGN.md L920 (tens of bytes per vessel), L930
//! (20 bytes or less per vessel update on average).
//!
//! Bits are appended least significant first: bit `i` of the stream is bit `i % 8` of byte `i / 8`, and a
//! multi-bit value is written low bit first. Varints are 7-bit groups, low group first, each followed by a
//! continuation bit, so a value under 128 costs 8 bits. Every read is bounds-checked; nothing here panics on
//! hostile input.
use crate::error::NetError;

/// Longest varint accepted: 10 groups carry 70 bits, enough for any u64.
const MAX_VARINT_GROUPS: u32 = 10;

/// Appends bits little-endian within bytes. Real constructor; packing is unit work.
#[derive(Clone, Debug, Default)]
pub struct BitWriter { pub(crate) bytes: Vec<u8>, pub(crate) bit: u32 }

impl BitWriter {
    pub fn new() -> Self { Self::default() }
    pub fn with_capacity(bytes: usize) -> Self { Self { bytes: Vec::with_capacity(bytes), bit: 0 } }
    /// Write the low `bits` (0..=64) of `value`. Unit: `net-schema`.
    pub fn write_bits(&mut self, value: u64, bits: u32) {
        let bits = bits.min(64);
        if bits == 0 { return; }
        let mut v = if bits == 64 { value } else { value & ((1u64 << bits) - 1) };
        let mut left = bits;
        while left > 0 {
            if self.bit == 0 { self.bytes.push(0); }
            let free = 8 - self.bit;
            let take = free.min(left);
            let chunk = (v & ((1u64 << take) - 1)) as u8;
            let last = self.bytes.len() - 1;
            self.bytes[last] |= chunk << self.bit;
            self.bit = (self.bit + take) % 8;
            v = if take >= 64 { 0 } else { v >> take };
            left -= take;
        }
    }
    pub fn write_bool(&mut self, v: bool) { self.write_bits(v as u64, 1) }
    /// Unsigned LEB-style varint in 7-bit groups (bit aligned). Unit: `net-schema`.
    pub fn write_varint(&mut self, v: u64) {
        let mut v = v;
        loop {
            let group = v & 0x7f;
            v >>= 7;
            self.write_bits(group, 7);
            self.write_bool(v != 0);
            if v == 0 { break; }
        }
    }
    /// Zig-zag signed varint. Unit: `net-schema`.
    pub fn write_svarint(&mut self, v: i64) { self.write_varint(zigzag(v)) }
    /// Zig-zag signed varint in 3-bit groups, each followed by a continuation bit: a delta of -4..=3 costs
    /// 4 bits, -32..=31 costs 8. Used for the small residuals of predicted poses and velocities, where most
    /// values are a handful of quantisation steps.
    pub(crate) fn write_small(&mut self, v: i64) {
        let mut v = zigzag(v);
        loop {
            let group = v & 0x7;
            v >>= 3;
            self.write_bits(group, 3);
            self.write_bool(v != 0);
            if v == 0 { break; }
        }
    }
    /// Bits written so far.
    pub fn bit_len(&self) -> usize { self.bytes.len() * 8 - if self.bit == 0 { 0 } else { 8 - self.bit as usize } }
    /// Pad to a byte boundary and return the bytes. Unit: `net-schema`.
    pub fn finish(self) -> Vec<u8> { self.bytes }

    /// Append every bit of `other` (used to put counts in front of items whose number is known only after the
    /// budget fill).
    pub(crate) fn append(&mut self, other: &BitWriter) {
        let total = other.bit_len();
        let full = total / 8;
        if self.bit == 0 {
            self.bytes.extend_from_slice(&other.bytes[..full]);
        } else {
            for &b in &other.bytes[..full] { self.write_bits(b as u64, 8); }
        }
        let rest = (total % 8) as u32;
        if rest > 0 { self.write_bits(other.bytes[full] as u64, rest); }
    }
    /// Drop everything after the first `bits` bits (rollback of an item that did not fit).
    pub(crate) fn truncate(&mut self, bits: usize) {
        if bits >= self.bit_len() { return; }
        let bytes = bits.div_ceil(8);
        self.bytes.truncate(bytes);
        self.bit = (bits % 8) as u32;
        if self.bit != 0 {
            let last = self.bytes.len() - 1;
            self.bytes[last] &= (1u8 << self.bit) - 1;
        }
    }
}

/// Bits a varint of `v` takes.
pub(crate) fn varint_bits(v: u64) -> usize {
    let mut groups = 1;
    let mut v = v >> 7;
    while v != 0 { groups += 1; v >>= 7; }
    groups * 8
}
pub(crate) fn svarint_bits(v: i64) -> usize { varint_bits(zigzag(v)) }
/// Bits [`BitWriter::write_small`] takes for `v`.
pub(crate) fn small_bits(v: i64) -> usize {
    let mut groups = 1;
    let mut z = zigzag(v) >> 3;
    while z != 0 { groups += 1; z >>= 3; }
    groups * 4
}
/// Longest small varint accepted: 22 groups carry 66 bits.
const MAX_SMALL_GROUPS: u32 = 22;

#[inline]
pub(crate) fn zigzag(v: i64) -> u64 { ((v << 1) ^ (v >> 63)) as u64 }
#[inline]
pub(crate) fn unzigzag(v: u64) -> i64 { ((v >> 1) as i64) ^ -((v & 1) as i64) }

/// Reads what [`BitWriter`] wrote; every read is bounds-checked and returns `NetError::Decode` on overrun.
#[derive(Clone, Debug)]
pub struct BitReader<'a> { pub(crate) bytes: &'a [u8], pub(crate) pos: usize }

impl<'a> BitReader<'a> {
    pub fn new(bytes: &'a [u8]) -> Self { Self { bytes, pos: 0 } }
    /// Unit: `net-schema`.
    pub fn read_bits(&mut self, bits: u32) -> Result<u64, NetError> {
        if bits > 64 { return Err(NetError::Decode(format!("read of {bits} bits"))); }
        if bits == 0 { return Ok(0); }
        if (bits as usize) > self.bits_left() { return Err(NetError::Decode("bit stream overrun".into())); }
        let mut out = 0u64;
        let mut got = 0u32;
        while got < bits {
            let byte = self.bytes[self.pos / 8];
            let off = (self.pos % 8) as u32;
            let avail = 8 - off;
            let take = avail.min(bits - got);
            let chunk = ((byte >> off) as u64) & ((1u64 << take) - 1);
            out |= chunk << got;
            got += take;
            self.pos += take as usize;
        }
        Ok(out)
    }
    pub fn read_bool(&mut self) -> Result<bool, NetError> { Ok(self.read_bits(1)? != 0) }
    /// Unit: `net-schema`.
    pub fn read_varint(&mut self) -> Result<u64, NetError> {
        let mut out = 0u64;
        for g in 0..MAX_VARINT_GROUPS {
            let group = self.read_bits(7)?;
            let shift = g * 7;
            if shift >= 64 || (shift == 63 && group > 1) {
                return Err(NetError::Decode("varint overflows u64".into()));
            }
            out |= group << shift;
            if !self.read_bool()? { return Ok(out); }
        }
        Err(NetError::Decode("varint too long".into()))
    }
    /// Unit: `net-schema`.
    pub fn read_svarint(&mut self) -> Result<i64, NetError> { Ok(unzigzag(self.read_varint()?)) }
    /// Read what [`BitWriter::write_small`] wrote.
    pub(crate) fn read_small(&mut self) -> Result<i64, NetError> {
        let mut out = 0u64;
        for g in 0..MAX_SMALL_GROUPS {
            let group = self.read_bits(3)?;
            let shift = g * 3;
            if shift >= 64 || (shift == 63 && group > 1) {
                return Err(NetError::Decode("small varint overflows u64".into()));
            }
            out |= group << shift;
            if !self.read_bool()? { return Ok(unzigzag(out)); }
        }
        Err(NetError::Decode("small varint too long".into()))
    }
    pub fn bits_left(&self) -> usize { (self.bytes.len() * 8).saturating_sub(self.pos) }

    /// Read a varint that must fit `u32`.
    pub(crate) fn read_varint_u32(&mut self) -> Result<u32, NetError> {
        u32::try_from(self.read_varint()?).map_err(|_| NetError::Decode("value out of u32 range".into()))
    }
    /// Read a count of elements that each take at least `min_bits`, refusing counts the remaining bits cannot
    /// hold (so a hostile count cannot make the decoder allocate gigabytes).
    pub(crate) fn read_count(&mut self, min_bits: usize) -> Result<usize, NetError> {
        let n = self.read_varint()?;
        let cap = (self.bits_left() / min_bits.max(1)) as u64;
        if n > cap { return Err(NetError::Decode(format!("count {n} exceeds the remaining {cap}"))); }
        Ok(n as usize)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bits_round_trip_all_widths() {
        let mut w = BitWriter::new();
        for bits in 0..=64u32 {
            let v = 0xDEAD_BEEF_F00D_CAFEu64.rotate_left(bits);
            w.write_bits(v, bits);
        }
        w.write_bool(true);
        let bytes = w.finish();
        let mut r = BitReader::new(&bytes);
        for bits in 0..=64u32 {
            let v = 0xDEAD_BEEF_F00D_CAFEu64.rotate_left(bits);
            let want = if bits == 64 { v } else if bits == 0 { 0 } else { v & ((1u64 << bits) - 1) };
            assert_eq!(r.read_bits(bits).unwrap(), want, "width {bits}");
        }
        assert!(r.read_bool().unwrap());
        assert!(r.read_bits(65).is_err());
    }

    #[test]
    fn varints_round_trip_and_reject_junk() {
        let vals = [0u64, 1, 127, 128, 300, 16_383, 16_384, u32::MAX as u64, u64::MAX];
        let svals = [0i64, -1, 1, -64, 63, -65, i32::MIN as i64, i64::MIN, i64::MAX];
        let mut w = BitWriter::new();
        for &v in &vals { w.write_varint(v); }
        for &v in &svals { w.write_svarint(v); }
        let bits = w.bit_len();
        let bytes = w.finish();
        let mut r = BitReader::new(&bytes);
        for &v in &vals { assert_eq!(r.read_varint().unwrap(), v); }
        for &v in &svals { assert_eq!(r.read_svarint().unwrap(), v); }
        assert!(bits <= bytes.len() * 8);
        // Every group continues: must stop after 10 groups, not loop or overflow.
        let junk = [0xffu8; 32];
        assert!(BitReader::new(&junk).read_varint().is_err());
        assert!(BitReader::new(&[]).read_varint().is_err());
        assert_eq!(varint_bits(127), 8);
        assert_eq!(varint_bits(128), 16);
    }

    #[test]
    fn small_varints_round_trip_and_reject_junk() {
        let vals = [0i64, -1, 1, -4, 3, 4, -5, -32, 31, 32, 1000, -77_777, i32::MIN as i64, i64::MIN, i64::MAX];
        let mut w = BitWriter::new();
        let mut want_bits = 0;
        for &v in &vals { w.write_small(v); want_bits += small_bits(v); }
        assert_eq!(w.bit_len(), want_bits);
        let bytes = w.finish();
        let mut r = BitReader::new(&bytes);
        for &v in &vals { assert_eq!(r.read_small().unwrap(), v); }
        assert_eq!(small_bits(3), 4);
        assert_eq!(small_bits(-4), 4);
        assert_eq!(small_bits(4), 8);
        assert!(BitReader::new(&[0xffu8; 32]).read_small().is_err());
        assert!(BitReader::new(&[]).read_small().is_err());
    }

    #[test]
    fn append_and_truncate() {
        let mut a = BitWriter::new();
        a.write_bits(0b101, 3);
        let mut b = BitWriter::new();
        b.write_bits(0x1234_5678, 32);
        b.write_bits(0b11, 2);
        a.append(&b);
        assert_eq!(a.bit_len(), 37);
        let mark = a.bit_len();
        a.write_bits(0xffff, 16);
        a.truncate(mark);
        assert_eq!(a.bit_len(), 37);
        a.write_bool(false);
        let bytes = a.finish();
        let mut r = BitReader::new(&bytes);
        assert_eq!(r.read_bits(3).unwrap(), 0b101);
        assert_eq!(r.read_bits(32).unwrap(), 0x1234_5678);
        assert_eq!(r.read_bits(2).unwrap(), 0b11);
        assert!(!r.read_bool().unwrap());
    }
}
