//! A serde serializer that feeds a [`StateHasher`]: any `Serialize` store hashes through its serde form, so
//! private columns and fields added later are covered without a second field list to keep in step.
//!
//! The byte stream is canonical and self-delimiting: integers little-endian at their own width, floats by
//! the hasher's NaN rule (`f32` widened exactly to `f64`), sequences, maps, strings and byte strings
//! prefixed with their length as `u64`, options and enum variants tagged. Fields marked `serde(skip)` are
//! derived caches and stay out. The serializer is not human-readable, so content hashes go in as raw bytes.
use super::StateHasher;
use serde::ser::{self, Serialize};

/// Bytes gathered before each hasher update; per-field updates would dominate the cost.
const BUFFER: usize = 1 << 14;

pub(crate) struct HashSer<'a> { hasher: &'a mut StateHasher, buf: Vec<u8> }

/// Hash a value through its serde form.
pub(crate) fn feed<T: Serialize + ?Sized>(hasher: &mut StateHasher, value: &T) {
    let mut s = HashSer { hasher, buf: Vec::with_capacity(BUFFER) };
    // The serializer itself never fails; an error can only come from a value's own `Serialize` impl, and
    // then the bytes fed so far still stand, plus a marker so the failure cannot pass for a clean hash.
    if value.serialize(&mut s).is_err() { s.put(&[0xEE; 8]); }
    s.flush();
}

impl HashSer<'_> {
    fn put(&mut self, bytes: &[u8]) {
        if self.buf.len() + bytes.len() > BUFFER { self.flush(); }
        if bytes.len() >= BUFFER { self.hasher.write_bytes(bytes); } else { self.buf.extend_from_slice(bytes); }
    }
    fn flush(&mut self) {
        if !self.buf.is_empty() { self.hasher.write_bytes(&self.buf); self.buf.clear(); }
    }
    fn float(&mut self, v: f64) {
        debug_assert!(!v.is_nan(), "NaN reached simulation state");
        let bits = if v.is_nan() { f64::NAN.to_bits() } else { v.to_bits() };
        self.put(&bits.to_le_bytes());
    }
    fn len(&mut self, n: usize) { self.put(&(n as u64).to_le_bytes()); }
}

/// The serializer has no failure of its own; this carries a message from a value's `Serialize` impl.
#[derive(Debug)]
pub(crate) struct Never(String);
impl core::fmt::Display for Never { fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result { f.write_str(&self.0) } }
impl std::error::Error for Never {}
impl ser::Error for Never { fn custom<T: core::fmt::Display>(msg: T) -> Self { Never(msg.to_string()) } }

type Done = Result<(), Never>;

impl<'a, 'b> ser::Serializer for &'b mut HashSer<'a> {
    type Ok = ();
    type Error = Never;
    type SerializeSeq = Counted<'a, 'b>;
    type SerializeTuple = Self;
    type SerializeTupleStruct = Self;
    type SerializeTupleVariant = Self;
    type SerializeMap = Counted<'a, 'b>;
    type SerializeStruct = Self;
    type SerializeStructVariant = Self;

    fn is_human_readable(&self) -> bool { false }
    fn serialize_bool(self, v: bool) -> Done { self.put(&[v as u8]); Ok(()) }
    fn serialize_i8(self, v: i8) -> Done { self.put(&v.to_le_bytes()); Ok(()) }
    fn serialize_i16(self, v: i16) -> Done { self.put(&v.to_le_bytes()); Ok(()) }
    fn serialize_i32(self, v: i32) -> Done { self.put(&v.to_le_bytes()); Ok(()) }
    fn serialize_i64(self, v: i64) -> Done { self.put(&v.to_le_bytes()); Ok(()) }
    fn serialize_i128(self, v: i128) -> Done { self.put(&v.to_le_bytes()); Ok(()) }
    fn serialize_u8(self, v: u8) -> Done { self.put(&[v]); Ok(()) }
    fn serialize_u16(self, v: u16) -> Done { self.put(&v.to_le_bytes()); Ok(()) }
    fn serialize_u32(self, v: u32) -> Done { self.put(&v.to_le_bytes()); Ok(()) }
    fn serialize_u64(self, v: u64) -> Done { self.put(&v.to_le_bytes()); Ok(()) }
    fn serialize_u128(self, v: u128) -> Done { self.put(&v.to_le_bytes()); Ok(()) }
    fn serialize_f32(self, v: f32) -> Done { self.float(v as f64); Ok(()) }
    fn serialize_f64(self, v: f64) -> Done { self.float(v); Ok(()) }
    fn serialize_char(self, v: char) -> Done { self.put(&(v as u32).to_le_bytes()); Ok(()) }
    fn serialize_str(self, v: &str) -> Done { self.len(v.len()); self.put(v.as_bytes()); Ok(()) }
    fn serialize_bytes(self, v: &[u8]) -> Done { self.len(v.len()); self.put(v); Ok(()) }
    fn serialize_none(self) -> Done { self.put(&[0]); Ok(()) }
    fn serialize_some<T: Serialize + ?Sized>(self, value: &T) -> Done { self.put(&[1]); value.serialize(self) }
    fn serialize_unit(self) -> Done { Ok(()) }
    fn serialize_unit_struct(self, _: &'static str) -> Done { Ok(()) }
    fn serialize_unit_variant(self, _: &'static str, index: u32, _: &'static str) -> Done { self.put(&index.to_le_bytes()); Ok(()) }
    fn serialize_newtype_struct<T: Serialize + ?Sized>(self, _: &'static str, value: &T) -> Done { value.serialize(self) }
    fn serialize_newtype_variant<T: Serialize + ?Sized>(self, _: &'static str, index: u32, _: &'static str, value: &T) -> Done { self.put(&index.to_le_bytes()); value.serialize(self) }
    fn serialize_seq(self, len: Option<usize>) -> Result<Counted<'a, 'b>, Never> { self.len(len.unwrap_or(usize::MAX)); Ok(Counted { ser: self, open: len.is_none() }) }
    fn serialize_tuple(self, _: usize) -> Result<Self, Never> { Ok(self) }
    fn serialize_tuple_struct(self, _: &'static str, _: usize) -> Result<Self, Never> { Ok(self) }
    fn serialize_tuple_variant(self, _: &'static str, index: u32, _: &'static str, _: usize) -> Result<Self, Never> { self.put(&index.to_le_bytes()); Ok(self) }
    fn serialize_map(self, len: Option<usize>) -> Result<Counted<'a, 'b>, Never> { self.len(len.unwrap_or(usize::MAX)); Ok(Counted { ser: self, open: len.is_none() }) }
    fn serialize_struct(self, _: &'static str, _: usize) -> Result<Self, Never> { Ok(self) }
    fn serialize_struct_variant(self, _: &'static str, index: u32, _: &'static str, _: usize) -> Result<Self, Never> { self.put(&index.to_le_bytes()); Ok(self) }
}

/// A sequence or map. One of known length was prefixed with it; one of unknown length (`open`) was prefixed
/// with `u64::MAX`, marks each entry and marks its end, so the stream stays delimited either way.
pub(crate) struct Counted<'a, 'b> { ser: &'b mut HashSer<'a>, open: bool }
impl ser::SerializeSeq for Counted<'_, '_> {
    type Ok = ();
    type Error = Never;
    fn serialize_element<T: Serialize + ?Sized>(&mut self, value: &T) -> Done { if self.open { self.ser.put(&[1]); } value.serialize(&mut *self.ser) }
    fn end(self) -> Done { if self.open { self.ser.put(&[0]); } Ok(()) }
}
impl ser::SerializeMap for Counted<'_, '_> {
    type Ok = ();
    type Error = Never;
    fn serialize_key<T: Serialize + ?Sized>(&mut self, key: &T) -> Done { if self.open { self.ser.put(&[1]); } key.serialize(&mut *self.ser) }
    fn serialize_value<T: Serialize + ?Sized>(&mut self, value: &T) -> Done { value.serialize(&mut *self.ser) }
    fn end(self) -> Done { if self.open { self.ser.put(&[0]); } Ok(()) }
}
impl<'a, 'b> ser::SerializeTuple for &'b mut HashSer<'a> {
    type Ok = ();
    type Error = Never;
    fn serialize_element<T: Serialize + ?Sized>(&mut self, value: &T) -> Done { value.serialize(&mut **self) }
    fn end(self) -> Done { Ok(()) }
}
impl<'a, 'b> ser::SerializeTupleStruct for &'b mut HashSer<'a> {
    type Ok = ();
    type Error = Never;
    fn serialize_field<T: Serialize + ?Sized>(&mut self, value: &T) -> Done { value.serialize(&mut **self) }
    fn end(self) -> Done { Ok(()) }
}
impl<'a, 'b> ser::SerializeTupleVariant for &'b mut HashSer<'a> {
    type Ok = ();
    type Error = Never;
    fn serialize_field<T: Serialize + ?Sized>(&mut self, value: &T) -> Done { value.serialize(&mut **self) }
    fn end(self) -> Done { Ok(()) }
}
impl<'a, 'b> ser::SerializeStruct for &'b mut HashSer<'a> {
    type Ok = ();
    type Error = Never;
    fn serialize_field<T: Serialize + ?Sized>(&mut self, _: &'static str, value: &T) -> Done { value.serialize(&mut **self) }
    fn end(self) -> Done { Ok(()) }
}
impl<'a, 'b> ser::SerializeStructVariant for &'b mut HashSer<'a> {
    type Ok = ();
    type Error = Never;
    fn serialize_field<T: Serialize + ?Sized>(&mut self, _: &'static str, value: &T) -> Done { value.serialize(&mut **self) }
    fn end(self) -> Done { Ok(()) }
}
