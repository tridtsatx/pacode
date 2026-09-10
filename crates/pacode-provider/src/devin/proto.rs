//! Lightweight protobuf wire codec addressing fields by number.
//!
//! Supports protobuf wire types 0 (varint), 1 (64-bit / double),
//! 2 (length-delimited string/bytes/message), and 5 (32-bit / float).
//! Unknown fields are skipped automatically by the [`Reader`] iterator.

use std::fmt;

/// Protobuf wire types supported by this codec.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WireType {
    Varint = 0,
    Fixed64 = 1,
    LengthDelimited = 2,
    Fixed32 = 5,
}

impl WireType {
    pub fn from_u8(val: u8) -> Result<Self, ProtoError> {
        match val {
            0 => Ok(WireType::Varint),
            1 => Ok(WireType::Fixed64),
            2 => Ok(WireType::LengthDelimited),
            5 => Ok(WireType::Fixed32),
            other => Err(ProtoError::InvalidWireType(other)),
        }
    }

    pub fn to_u8(self) -> u8 {
        match self {
            WireType::Varint => 0,
            WireType::Fixed64 => 1,
            WireType::LengthDelimited => 2,
            WireType::Fixed32 => 5,
        }
    }
}

#[derive(Debug, thiserror::Error, PartialEq, Eq)]
pub enum ProtoError {
    #[error("unexpected end of buffer")]
    UnexpectedEof,
    #[error("invalid varint")]
    InvalidVarint,
    #[error("invalid wire type: {0}")]
    InvalidWireType(u8),
    #[error("invalid UTF-8 string: {0}")]
    InvalidUtf8(#[from] std::str::Utf8Error),
    #[error("type mismatch: {0}")]
    TypeMismatch(&'static str),
}

/// Encode a 64-bit integer into varint (LEB128) format.
pub fn encode_varint(mut val: u64, buf: &mut Vec<u8>) {
    while val >= 0x80 {
        buf.push((val as u8 & 0x7f) | 0x80);
        val >>= 7;
    }
    buf.push(val as u8);
}

/// Decode a varint (LEB128) from `buf`, returning `(value, bytes_consumed)`.
pub fn decode_varint(buf: &[u8]) -> Result<(u64, usize), ProtoError> {
    let mut val = 0u64;
    let mut shift = 0;
    for (i, &b) in buf.iter().enumerate() {
        if i >= 10 {
            return Err(ProtoError::InvalidVarint);
        }
        if i == 9 && (b & 0x7f) > 1 {
            return Err(ProtoError::InvalidVarint);
        }
        val |= ((b & 0x7f) as u64) << shift;
        if (b & 0x80) == 0 {
            return Ok((val, i + 1));
        }
        shift += 7;
    }
    Err(ProtoError::UnexpectedEof)
}

/// Append fields in wire format.
#[derive(Default, Debug, Clone)]
pub struct Writer {
    buf: Vec<u8>,
}

impl Writer {
    pub fn new() -> Self {
        Self { buf: Vec::new() }
    }

    pub fn with_capacity(capacity: usize) -> Self {
        Self {
            buf: Vec::with_capacity(capacity),
        }
    }

    pub fn write_tag(&mut self, field_no: u32, wire_type: WireType) {
        let key = ((field_no as u64) << 3) | (wire_type.to_u8() as u64);
        encode_varint(key, &mut self.buf);
    }

    pub fn write_varint(&mut self, field_no: u32, val: u64) {
        self.write_tag(field_no, WireType::Varint);
        encode_varint(val, &mut self.buf);
    }

    pub fn write_fixed64(&mut self, field_no: u32, val: u64) {
        self.write_tag(field_no, WireType::Fixed64);
        self.buf.extend_from_slice(&val.to_le_bytes());
    }

    pub fn write_double(&mut self, field_no: u32, val: f64) {
        self.write_fixed64(field_no, val.to_bits());
    }

    pub fn write_fixed32(&mut self, field_no: u32, val: u32) {
        self.write_tag(field_no, WireType::Fixed32);
        self.buf.extend_from_slice(&val.to_le_bytes());
    }

    pub fn write_float(&mut self, field_no: u32, val: f32) {
        self.write_fixed32(field_no, val.to_bits());
    }

    pub fn write_bytes(&mut self, field_no: u32, bytes: &[u8]) {
        self.write_tag(field_no, WireType::LengthDelimited);
        encode_varint(bytes.len() as u64, &mut self.buf);
        self.buf.extend_from_slice(bytes);
    }

    pub fn write_string(&mut self, field_no: u32, s: &str) {
        self.write_bytes(field_no, s.as_bytes());
    }

    pub fn write_message(&mut self, field_no: u32, msg: &Writer) {
        self.write_bytes(field_no, msg.as_bytes());
    }

    pub fn as_bytes(&self) -> &[u8] {
        &self.buf
    }

    pub fn into_bytes(self) -> Vec<u8> {
        self.buf
    }

    pub fn len(&self) -> usize {
        self.buf.len()
    }

    pub fn is_empty(&self) -> bool {
        self.buf.is_empty()
    }
}

/// Decoded protobuf field value referencing the source buffer.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum WireValue<'a> {
    Varint(u64),
    Fixed64([u8; 8]),
    LengthDelimited(&'a [u8]),
    Fixed32([u8; 4]),
}

impl<'a> WireValue<'a> {
    pub fn as_varint(&self) -> Result<u64, ProtoError> {
        match self {
            WireValue::Varint(v) => Ok(*v),
            _ => Err(ProtoError::TypeMismatch("expected Varint")),
        }
    }

    pub fn as_str(&self) -> Result<&'a str, ProtoError> {
        match self {
            WireValue::LengthDelimited(bytes) => {
                std::str::from_utf8(bytes).map_err(ProtoError::from)
            }
            _ => Err(ProtoError::TypeMismatch("expected LengthDelimited string")),
        }
    }

    pub fn as_bytes(&self) -> Result<&'a [u8], ProtoError> {
        match self {
            WireValue::LengthDelimited(bytes) => Ok(bytes),
            _ => Err(ProtoError::TypeMismatch("expected LengthDelimited bytes")),
        }
    }

    pub fn as_message(&self) -> Result<Reader<'a>, ProtoError> {
        match self {
            WireValue::LengthDelimited(bytes) => Ok(Reader::new(bytes)),
            _ => Err(ProtoError::TypeMismatch("expected LengthDelimited message")),
        }
    }

    pub fn as_double(&self) -> Result<f64, ProtoError> {
        match self {
            WireValue::Fixed64(bytes) => Ok(f64::from_le_bytes(*bytes)),
            _ => Err(ProtoError::TypeMismatch("expected Fixed64 double")),
        }
    }

    pub fn as_float(&self) -> Result<f32, ProtoError> {
        match self {
            WireValue::Fixed32(bytes) => Ok(f32::from_le_bytes(*bytes)),
            _ => Err(ProtoError::TypeMismatch("expected Fixed32 float")),
        }
    }
}

/// Incremental protobuf stream reader iterating over `(field_no, wire_type, value)`.
#[derive(Clone)]
pub struct Reader<'a> {
    data: &'a [u8],
    offset: usize,
}

impl<'a> Reader<'a> {
    pub fn new(data: &'a [u8]) -> Self {
        Self { data, offset: 0 }
    }

    pub fn remaining(&self) -> &'a [u8] {
        &self.data[self.offset..]
    }

    pub fn is_empty(&self) -> bool {
        self.offset >= self.data.len()
    }

    pub fn next_field(&mut self) -> Result<Option<(u32, WireType, WireValue<'a>)>, ProtoError> {
        if self.offset >= self.data.len() {
            return Ok(None);
        }

        let (key, n) = decode_varint(&self.data[self.offset..])?;
        self.offset += n;

        let wire_type_u8 = (key & 0x07) as u8;
        let wire_type = WireType::from_u8(wire_type_u8)?;
        let field_no = (key >> 3) as u32;

        let value = match wire_type {
            WireType::Varint => {
                let (v, vn) = decode_varint(&self.data[self.offset..])?;
                self.offset += vn;
                WireValue::Varint(v)
            }
            WireType::Fixed64 => {
                if self.offset + 8 > self.data.len() {
                    return Err(ProtoError::UnexpectedEof);
                }
                let mut bytes = [0u8; 8];
                bytes.copy_from_slice(&self.data[self.offset..self.offset + 8]);
                self.offset += 8;
                WireValue::Fixed64(bytes)
            }
            WireType::LengthDelimited => {
                let (len, ln) = decode_varint(&self.data[self.offset..])?;
                self.offset += ln;
                let len = len as usize;
                if self.offset + len > self.data.len() {
                    return Err(ProtoError::UnexpectedEof);
                }
                let slice = &self.data[self.offset..self.offset + len];
                self.offset += len;
                WireValue::LengthDelimited(slice)
            }
            WireType::Fixed32 => {
                if self.offset + 4 > self.data.len() {
                    return Err(ProtoError::UnexpectedEof);
                }
                let mut bytes = [0u8; 4];
                bytes.copy_from_slice(&self.data[self.offset..self.offset + 4]);
                self.offset += 4;
                WireValue::Fixed32(bytes)
            }
        };

        Ok(Some((field_no, wire_type, value)))
    }
}

impl<'a> Iterator for Reader<'a> {
    type Item = Result<(u32, WireType, WireValue<'a>), ProtoError>;

    fn next(&mut self) -> Option<Self::Item> {
        match self.next_field() {
            Ok(Some(item)) => Some(Ok(item)),
            Ok(None) => None,
            Err(e) => {
                self.offset = self.data.len();
                Some(Err(e))
            }
        }
    }
}

impl fmt::Debug for Reader<'_> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Reader")
            .field("offset", &self.offset)
            .field("total_len", &self.data.len())
            .finish()
    }
}
