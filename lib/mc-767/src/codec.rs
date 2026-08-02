//! 网络读写原语：VarInt/VarLong、大端数字、字符串、UUID、BitSet 等。
//!
//! 所有网络字符串使用 UTF-8（u16 长度上限），与 1.21.1 `PacketByteBuf` 一致；
//! NBT 字符串使用 CESU-8（见 [`crate::nbt`]）。

use std::fmt;

use uuid::Uuid;

use crate::nbt::{self, Nbt};

/// 编解码错误。
#[derive(Debug)]
pub enum CodecError {
    /// 数据不足。
    UnexpectedEof { wanted: usize, available: usize },
    /// `VarInt` 超过 5 字节。
    VarIntTooLong,
    /// `VarLong` 超过 10 字节。
    VarLongTooLong,
    /// 字符串非法 UTF-8。
    InvalidUtf8(std::str::Utf8Error),
    /// 字符串超过上限。
    StringTooLong { len: usize, max: usize },
    /// 字段长度非法。
    InvalidLength { field: &'static str, len: usize },
    /// 底层 NBT 错误。
    Nbt(nbt::NbtError),
}

impl fmt::Display for CodecError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::UnexpectedEof { wanted, available } => {
                write!(f, "unexpected eof: wanted {wanted} bytes, have {available}")
            }
            Self::VarIntTooLong => f.write_str("varint exceeded 5 bytes"),
            Self::VarLongTooLong => f.write_str("varlong exceeded 10 bytes"),
            Self::InvalidUtf8(err) => write!(f, "invalid utf-8: {err}"),
            Self::StringTooLong { len, max } => write!(f, "string length {len} exceeds {max}"),
            Self::InvalidLength { field, len } => write!(f, "invalid length for {field}: {len}"),
            Self::Nbt(err) => write!(f, "nbt: {err}"),
        }
    }
}

impl std::error::Error for CodecError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::InvalidUtf8(err) => Some(err),
            Self::Nbt(err) => Some(err),
            _ => None,
        }
    }
}

/// 网络字符串最大长度（1.21.1 为 32767 个 UTF-16 码元）。
pub const MAX_STRING_LEN: usize = 32767;

/// 只读字节游标。
pub struct Reader<'a> {
    data: &'a [u8],
    pos: usize,
}

impl<'a> Reader<'a> {
    /// 在 `data` 上创建游标。
    #[must_use]
    pub const fn new(data: &'a [u8]) -> Self {
        Self { data, pos: 0 }
    }

    /// 剩余可读字节数。
    #[must_use]
    pub const fn remaining(&self) -> usize {
        self.data.len() - self.pos
    }

    /// 是否已读尽。
    #[must_use]
    pub const fn is_empty(&self) -> bool {
        self.remaining() == 0
    }

    /// 已消费字节数。
    #[must_use]
    pub const fn position(&self) -> usize {
        self.pos
    }

    pub(crate) fn take(&mut self, count: usize) -> Result<&'a [u8], CodecError> {
        let end = self
            .pos
            .checked_add(count)
            .ok_or(CodecError::InvalidLength {
                field: "take",
                len: count,
            })?;
        if end > self.data.len() {
            return Err(CodecError::UnexpectedEof {
                wanted: count,
                available: self.remaining(),
            });
        }
        let slice = &self.data[self.pos..end];
        self.pos = end;
        Ok(slice)
    }

    /// 读取 u8。
    pub fn read_u8(&mut self) -> Result<u8, CodecError> {
        Ok(self.take(1)?[0])
    }

    /// 读取 i8。
    pub fn read_i8(&mut self) -> Result<i8, CodecError> {
        Ok(self.read_u8()? as i8)
    }

    /// 读取大端 u16。
    pub fn read_u16_be(&mut self) -> Result<u16, CodecError> {
        let bytes = self.take(2)?;
        Ok(u16::from_be_bytes([bytes[0], bytes[1]]))
    }

    /// 读取大端 i16。
    pub fn read_i16_be(&mut self) -> Result<i16, CodecError> {
        Ok(self.read_u16_be()? as i16)
    }

    /// 读取大端 i32。
    pub fn read_i32_be(&mut self) -> Result<i32, CodecError> {
        let bytes = self.take(4)?;
        Ok(i32::from_be_bytes([bytes[0], bytes[1], bytes[2], bytes[3]]))
    }

    /// 读取大端 i64。
    pub fn read_i64_be(&mut self) -> Result<i64, CodecError> {
        let bytes = self.take(8)?;
        Ok(i64::from_be_bytes([
            bytes[0], bytes[1], bytes[2], bytes[3], bytes[4], bytes[5], bytes[6], bytes[7],
        ]))
    }

    /// 读取大端 f32。
    pub fn read_f32_be(&mut self) -> Result<f32, CodecError> {
        Ok(f32::from_bits(self.read_u32_be()?))
    }

    fn read_u32_be(&mut self) -> Result<u32, CodecError> {
        let bytes = self.take(4)?;
        Ok(u32::from_be_bytes([bytes[0], bytes[1], bytes[2], bytes[3]]))
    }

    /// 读取大端 f64。
    pub fn read_f64_be(&mut self) -> Result<f64, CodecError> {
        Ok(f64::from_bits(self.read_u64_be()?))
    }

    fn read_u64_be(&mut self) -> Result<u64, CodecError> {
        let bytes = self.take(8)?;
        Ok(u64::from_be_bytes([
            bytes[0], bytes[1], bytes[2], bytes[3], bytes[4], bytes[5], bytes[6], bytes[7],
        ]))
    }

    /// 读取布尔（非 0 为 true）。
    pub fn read_bool(&mut self) -> Result<bool, CodecError> {
        Ok(self.read_u8()? != 0)
    }

    /// 读取 `VarInt`。
    pub fn read_var_int(&mut self) -> Result<i32, CodecError> {
        let mut value = 0u32;
        for shift in (0..35).step_by(7) {
            let byte = self.read_u8()?;
            value |= u32::from(byte & 0x7F) << shift;
            if byte & 0x80 == 0 {
                return Ok(value as i32);
            }
        }
        Err(CodecError::VarIntTooLong)
    }

    /// 读取 `VarLong`。
    pub fn read_var_long(&mut self) -> Result<i64, CodecError> {
        let mut value = 0u64;
        for shift in (0..70).step_by(7) {
            let byte = self.read_u8()?;
            value |= u64::from(byte & 0x7F) << shift;
            if byte & 0x80 == 0 {
                return Ok(value as i64);
            }
        }
        Err(CodecError::VarLongTooLong)
    }

    /// 读取长度前缀 UTF-8 字符串（VarInt 长度，上限 32767）。
    pub fn read_string(&mut self) -> Result<String, CodecError> {
        let len = usize::try_from(self.read_var_int()?).map_err(|_| CodecError::InvalidLength {
            field: "string length",
            len: 0,
        })?;
        if len > MAX_STRING_LEN {
            return Err(CodecError::StringTooLong {
                len,
                max: MAX_STRING_LEN,
            });
        }
        let bytes = self.take(len)?;
        std::str::from_utf8(bytes)
            .map(str::to_owned)
            .map_err(CodecError::InvalidUtf8)
    }

    /// 读取长度前缀字节数组（VarInt 长度）。
    pub fn read_byte_array(&mut self) -> Result<&'a [u8], CodecError> {
        let len = usize::try_from(self.read_var_int()?).map_err(|_| CodecError::InvalidLength {
            field: "byte array length",
            len: 0,
        })?;
        self.take(len)
    }

    /// 读取固定数量的原始字节。
    pub fn read_bytes(&mut self, count: usize) -> Result<&'a [u8], CodecError> {
        self.take(count)
    }

    /// 读取 16 字节 UUID。
    pub fn read_uuid(&mut self) -> Result<Uuid, CodecError> {
        let bytes = self.take(16)?;
        let mut arr = [0u8; 16];
        arr.copy_from_slice(bytes);
        Ok(Uuid::from_bytes(arr))
    }

    /// 读取 BitSet（VarInt 长度 + i64 数组）。
    pub fn read_bitset(&mut self) -> Result<Vec<i64>, CodecError> {
        let len = usize::try_from(self.read_var_int()?).map_err(|_| CodecError::InvalidLength {
            field: "bitset length",
            len: 0,
        })?;
        let mut values = Vec::with_capacity(len);
        for _ in 0..len {
            values.push(self.read_i64_be()?);
        }
        Ok(values)
    }

    /// 读取未命名 NBT 根标签；根为 End(0) 时返回 `None`。
    pub fn read_nbt(&mut self) -> Result<Option<Nbt>, CodecError> {
        nbt::decode_unnamed(self).map_err(CodecError::Nbt)
    }

    /// 读取全部剩余字节。
    pub fn read_rest(&mut self) -> &'a [u8] {
        let rest = &self.data[self.pos..];
        self.pos = self.data.len();
        rest
    }
}

/// 可增长的字节写出器。
#[derive(Default)]
pub struct Writer {
    buf: Vec<u8>,
}

impl Writer {
    /// 创建空写出器。
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// 带容量创建。
    #[must_use]
    pub fn with_capacity(capacity: usize) -> Self {
        Self {
            buf: Vec::with_capacity(capacity),
        }
    }

    /// 已写出字节数。
    #[must_use]
    pub const fn len(&self) -> usize {
        self.buf.len()
    }

    /// 是否为空。
    #[must_use]
    pub const fn is_empty(&self) -> bool {
        self.buf.is_empty()
    }

    /// 取走缓冲区。
    #[must_use]
    pub fn into_bytes(self) -> Vec<u8> {
        self.buf
    }

    /// 追加原始字节。
    pub fn write_slice(&mut self, bytes: &[u8]) {
        self.buf.extend_from_slice(bytes);
    }

    /// 写入 u8。
    pub fn write_u8(&mut self, value: u8) {
        self.buf.push(value);
    }

    /// 写入 i8。
    pub fn write_i8(&mut self, value: i8) {
        self.buf.push(value as u8);
    }

    /// 写入大端 u16。
    pub fn write_u16_be(&mut self, value: u16) {
        self.buf.extend_from_slice(&value.to_be_bytes());
    }

    /// 写入大端 i16。
    pub fn write_i16_be(&mut self, value: i16) {
        self.buf.extend_from_slice(&value.to_be_bytes());
    }

    /// 写入大端 i32。
    pub fn write_i32_be(&mut self, value: i32) {
        self.buf.extend_from_slice(&value.to_be_bytes());
    }

    /// 写入大端 i64。
    pub fn write_i64_be(&mut self, value: i64) {
        self.buf.extend_from_slice(&value.to_be_bytes());
    }

    /// 写入大端 f32。
    pub fn write_f32_be(&mut self, value: f32) {
        self.buf.extend_from_slice(&value.to_be_bytes());
    }

    /// 写入大端 f64。
    pub fn write_f64_be(&mut self, value: f64) {
        self.buf.extend_from_slice(&value.to_be_bytes());
    }

    /// 写入布尔。
    pub fn write_bool(&mut self, value: bool) {
        self.write_u8(u8::from(value));
    }

    /// 写入 VarInt（5 字节封顶，负数按 5 字节）。
    pub fn write_var_int(&mut self, value: i32) {
        let mut val = value as u32;
        loop {
            let byte = (val & 0x7F) as u8;
            val >>= 7;
            if val == 0 {
                self.buf.push(byte);
                break;
            }
            self.buf.push(byte | 0x80);
        }
    }

    /// 写入 `VarLong`。
    pub fn write_var_long(&mut self, value: i64) {
        let mut val = value as u64;
        loop {
            let byte = (val & 0x7F) as u8;
            val >>= 7;
            if val == 0 {
                self.buf.push(byte);
                break;
            }
            self.buf.push(byte | 0x80);
        }
    }

    /// 写入长度前缀 UTF-8 字符串。
    pub fn write_string(&mut self, value: &str) {
        self.write_var_int(i32::try_from(value.len()).expect("string exceeds i32"));
        self.write_slice(value.as_bytes());
    }

    /// 写入长度前缀字节数组。
    pub fn write_byte_array(&mut self, value: &[u8]) {
        self.write_var_int(i32::try_from(value.len()).expect("array exceeds i32"));
        self.write_slice(value);
    }

    /// 写入 16 字节 UUID。
    pub fn write_uuid(&mut self, value: &Uuid) {
        self.write_slice(value.as_bytes());
    }

    /// 写入 BitSet（VarInt 长度 + i64 数组）。
    pub fn write_bitset(&mut self, values: &[i64]) {
        self.write_var_int(i32::try_from(values.len()).expect("bitset exceeds i32"));
        for value in values {
            self.write_i64_be(*value);
        }
    }

    /// 写入未命名 NBT 根标签。
    pub fn write_nbt_unnamed(&mut self, value: &Nbt) -> Result<(), CodecError> {
        nbt::encode_unnamed(value, self).map_err(CodecError::Nbt)
    }
}

#[cfg(test)]
mod tests {
    use super::{Reader, Writer};

    #[test]
    fn var_int_round_trip() {
        for value in [0, 1, 127, 128, 255, 300, 2147483647, -1, -2147483648] {
            let mut w = Writer::new();
            w.write_var_int(value);
            let bytes = w.into_bytes();
            let mut r = Reader::new(&bytes);
            assert_eq!(r.read_var_int().unwrap(), value);
            assert!(r.is_empty());
        }
    }

    #[test]
    fn var_long_round_trip() {
        for value in [0i64, 1, 127, 128, 300, i64::MAX, -1, i64::MIN] {
            let mut w = Writer::new();
            w.write_var_long(value);
            let bytes = w.into_bytes();
            let mut r = Reader::new(&bytes);
            assert_eq!(r.read_var_long().unwrap(), value);
            assert!(r.is_empty());
        }
    }

    #[test]
    fn string_round_trip() {
        let mut w = Writer::new();
        w.write_string("minecraft:overworld");
        let bytes = w.into_bytes();
        let mut r = Reader::new(&bytes);
        assert_eq!(r.read_string().unwrap(), "minecraft:overworld");
    }

    #[test]
    fn var_int_canonical_encoding() {
        let mut w = Writer::new();
        w.write_var_int(300);
        assert_eq!(w.into_bytes(), vec![0xAC, 0x02]);
    }

    #[test]
    fn eof_errors() {
        let mut r = Reader::new(&[0x01]);
        assert!(matches!(
            r.read_i32_be(),
            Err(super::CodecError::UnexpectedEof { .. })
        ));
    }
}
