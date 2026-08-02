//! NBT 标签模型与 Java Edition 编解码。
//!
//! 两种根格式：
//! - **命名**（Anvil 存档）：根复合先写标签字节 + 名称。
//! - **未命名**（网络，如 chunk heightmap、registry data）：根复合直接写条目 + End。
//!
//! 字符串使用 Java 修改版 UTF-8（CESU-8），u16 长度前缀。

use std::fmt;

use crate::codec::{Reader, Writer};

/// NBT 标签类型 ID。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u8)]
pub enum TagId {
    End = 0,
    Byte = 1,
    Short = 2,
    Int = 3,
    Long = 4,
    Float = 5,
    Double = 6,
    ByteArray = 7,
    String = 8,
    List = 9,
    Compound = 10,
    IntArray = 11,
    LongArray = 12,
}

impl TagId {
    /// 从标签字节解析；未知字节返回 `None`。
    #[must_use]
    pub const fn from_id(id: u8) -> Option<Self> {
        match id {
            0 => Some(Self::End),
            1 => Some(Self::Byte),
            2 => Some(Self::Short),
            3 => Some(Self::Int),
            4 => Some(Self::Long),
            5 => Some(Self::Float),
            6 => Some(Self::Double),
            7 => Some(Self::ByteArray),
            8 => Some(Self::String),
            9 => Some(Self::List),
            10 => Some(Self::Compound),
            11 => Some(Self::IntArray),
            12 => Some(Self::LongArray),
            _ => None,
        }
    }

    /// 标签类型 ID 字节。
    #[must_use]
    pub const fn id(self) -> u8 {
        self as u8
    }
}

/// NBT 标签值。
///
/// 递归变体装箱以保持枚举体积稳定；`Compound` 保持插入顺序（对存档往返
/// 确定性有用）。
#[derive(Debug, Clone, PartialEq)]
pub enum Nbt {
    Byte(i8),
    Short(i16),
    Int(i32),
    Long(i64),
    Float(f32),
    Double(f64),
    ByteArray(Box<[i8]>),
    String(String),
    List(Box<NbtList>),
    Compound(Box<NbtCompound>),
    IntArray(Box<[i32]>),
    LongArray(Box<[i64]>),
}

/// NBT 列表（同构元素）。
#[derive(Debug, Clone, PartialEq)]
pub struct NbtList {
    /// 元素标签类型（空列表为 End）。
    pub tag: TagId,
    /// 元素值。
    pub values: Vec<Nbt>,
}

impl NbtList {
    /// 创建空列表。
    #[must_use]
    pub const fn empty() -> Self {
        Self {
            tag: TagId::End,
            values: Vec::new(),
        }
    }

    /// 从元素构造（自动推导元素标签类型；空列表为 End）。
    #[must_use]
    pub fn from_values(values: Vec<Nbt>) -> Self {
        let tag = values.first().map_or(TagId::End, |v| {
            TagId::from_id(tag_id_of(v)).expect("known tag id")
        });
        Self { tag, values }
    }

    /// 追加元素（自动设置/校验元素标签类型）。
    pub fn push(&mut self, value: Nbt) {
        if self.values.is_empty() {
            self.tag = TagId::from_id(tag_id_of(&value)).expect("known tag id");
        } else {
            debug_assert_eq!(
                self.tag,
                TagId::from_id(tag_id_of(&value)).expect("known tag id"),
                "NbtList 元素类型必须一致"
            );
        }
        self.values.push(value);
    }
}

/// NBT 复合（有序键值）。
#[derive(Debug, Clone, PartialEq, Default)]
pub struct NbtCompound {
    /// 有序条目。
    pub entries: Vec<(String, Nbt)>,
}

impl NbtCompound {
    /// 创建空复合。
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// 插入条目；键已存在时替换值（保持键的顺序位置）。
    pub fn insert(&mut self, key: impl Into<String>, value: Nbt) {
        let key = key.into();
        if let Some(entry) = self.entries.iter_mut().find(|(k, _)| *k == key) {
            entry.1 = value;
        } else {
            self.entries.push((key, value));
        }
    }

    /// 按键查找。
    #[must_use]
    pub fn get(&self, key: &str) -> Option<&Nbt> {
        self.entries.iter().find(|(k, _)| k == key).map(|(_, v)| v)
    }

    /// 取字符串值。
    #[must_use]
    pub fn get_string(&self, key: &str) -> Option<&str> {
        match self.get(key) {
            Some(Nbt::String(s)) => Some(s),
            _ => None,
        }
    }

    /// 取 i64 值（接受 Byte/Short/Int/Long）。
    #[must_use]
    pub fn get_i64(&self, key: &str) -> Option<i64> {
        match self.get(key) {
            Some(Nbt::Byte(v)) => Some(i64::from(*v)),
            Some(Nbt::Short(v)) => Some(i64::from(*v)),
            Some(Nbt::Int(v)) => Some(i64::from(*v)),
            Some(Nbt::Long(v)) => Some(*v),
            _ => None,
        }
    }

    /// 取 i32 值（接受 Byte/Short/Int/Long）。
    #[must_use]
    pub fn get_i32(&self, key: &str) -> Option<i32> {
        match self.get(key) {
            Some(Nbt::Byte(v)) => Some(i32::from(*v)),
            Some(Nbt::Short(v)) => Some(i32::from(*v)),
            Some(Nbt::Int(v)) => Some(*v),
            Some(Nbt::Long(v)) => i32::try_from(*v).ok(),
            _ => None,
        }
    }

    /// 取布尔（Byte 非 0）。
    #[must_use]
    pub fn get_bool(&self, key: &str) -> Option<bool> {
        match self.get(key) {
            Some(Nbt::Byte(v)) => Some(*v != 0),
            _ => None,
        }
    }

    /// 取复合值。
    #[must_use]
    pub fn get_compound(&self, key: &str) -> Option<&Self> {
        match self.get(key) {
            Some(Nbt::Compound(c)) => Some(c),
            _ => None,
        }
    }

    /// 取列表值。
    #[must_use]
    pub fn get_list(&self, key: &str) -> Option<&NbtList> {
        match self.get(key) {
            Some(Nbt::List(l)) => Some(l),
            _ => None,
        }
    }

    /// 取长整型数组。
    #[must_use]
    pub fn get_long_array(&self, key: &str) -> Option<&[i64]> {
        match self.get(key) {
            Some(Nbt::LongArray(a)) => Some(a),
            _ => None,
        }
    }
}

/// NBT 编解码错误。
#[derive(Debug)]
pub enum NbtError {
    /// 读取失败。
    Eof,
    /// 非法标签 ID。
    InvalidTagId(u8),
    /// 非法列表元素标签（非 End 的列表元素必须是复合）。
    InvalidListTag(TagId),
    /// 字符串过长。
    StringTooLong(usize),
    /// 无效 CESU-8。
    InvalidCesu8,
    /// 递归深度超限。
    DepthLimit,
    /// 未命名根标签不是复合。
    NonCompoundRoot,
}

impl fmt::Display for NbtError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Eof => f.write_str("unexpected eof"),
            Self::InvalidTagId(id) => write!(f, "invalid tag id: {id}"),
            Self::InvalidListTag(tag) => write!(f, "list element must be compound, got {tag:?}"),
            Self::StringTooLong(len) => write!(f, "string length {len} exceeds u16"),
            Self::InvalidCesu8 => f.write_str("invalid cesu-8"),
            Self::DepthLimit => f.write_str("nesting depth exceeded"),
            Self::NonCompoundRoot => f.write_str("unnamed root must be a compound"),
        }
    }
}

impl std::error::Error for NbtError {}

const MAX_DEPTH: usize = 64;

fn read_tag_id(reader: &mut Reader<'_>) -> Result<TagId, NbtError> {
    let id = reader.read_u8().map_err(|_| NbtError::Eof)?;
    TagId::from_id(id).ok_or(NbtError::InvalidTagId(id))
}

/// CESU-8 编码（Java 修改版 UTF-8；BMP 字符与 UTF-8 相同，增补平面编码为
/// 代理对 6 字节）。
fn encode_cesu8(value: &str, out: &mut Vec<u8>) {
    for ch in value.chars() {
        let mut buf = [0u8; 4];
        let encoded = ch.encode_utf8(&mut buf);
        match ch {
            '\u{0000}' => out.extend_from_slice(&[0xC0, 0x80]),
            '\u{0001}'..='\u{FFFF}' => out.extend_from_slice(encoded.as_bytes()),
            _ => {
                // 增补平面：拆成代理对
                let mut units = [0u16; 2];
                let written = ch.encode_utf16(&mut units);
                for &unit in written.iter() {
                    let b0 = 0xE0 | ((unit >> 12) as u8);
                    let b1 = 0x80 | (((unit >> 6) & 0x3F) as u8);
                    let b2 = 0x80 | ((unit & 0x3F) as u8);
                    out.extend_from_slice(&[b0, b1, b2]);
                }
            }
        }
    }
}

/// 解码 CESU-8 字节为 String（假定不会出现增补平面代理对之外的非法序列）。
fn decode_cesu8(bytes: &[u8]) -> Result<String, NbtError> {
    let mut units: Vec<u16> = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        let b0 = bytes[i];
        if b0 < 0x80 {
            units.push(u16::from(b0));
            i += 1;
        } else if (0xC0..0xE0).contains(&b0) {
            if i + 1 >= bytes.len() {
                return Err(NbtError::InvalidCesu8);
            }
            let b1 = bytes[i + 1];
            if b1 & 0xC0 != 0x80 {
                return Err(NbtError::InvalidCesu8);
            }
            units.push(u16::from(b0 & 0x1F) << 6 | u16::from(b1 & 0x3F));
            i += 2;
        } else if (0xE0..0xF0).contains(&b0) {
            if i + 2 >= bytes.len() {
                return Err(NbtError::InvalidCesu8);
            }
            let b1 = bytes[i + 1];
            let b2 = bytes[i + 2];
            if b1 & 0xC0 != 0x80 || b2 & 0xC0 != 0x80 {
                return Err(NbtError::InvalidCesu8);
            }
            units.push(
                u16::from(b0 & 0x0F) << 12 | u16::from(b1 & 0x3F) << 6 | u16::from(b2 & 0x3F),
            );
            i += 3;
        } else {
            return Err(NbtError::InvalidCesu8);
        }
    }
    String::from_utf16(&units).map_err(|_| NbtError::InvalidCesu8)
}

fn read_string(reader: &mut Reader<'_>) -> Result<String, NbtError> {
    let len = usize::from(reader.read_u16_be().map_err(|_| NbtError::Eof)?);
    if len > u16::MAX as usize {
        return Err(NbtError::StringTooLong(len));
    }
    let bytes = reader.take(len).map_err(|_| NbtError::Eof)?;
    decode_cesu8(bytes)
}

fn write_string(writer: &mut Writer, value: &str) {
    let mut buf = Vec::with_capacity(value.len());
    encode_cesu8(value, &mut buf);
    writer.write_u16_be(u16::try_from(buf.len()).expect("nbt string exceeds u16"));
    writer.write_slice(&buf);
}

fn read_payload(reader: &mut Reader<'_>, tag: TagId, depth: usize) -> Result<Nbt, NbtError> {
    if depth > MAX_DEPTH {
        return Err(NbtError::DepthLimit);
    }
    match tag {
        TagId::Byte => Ok(Nbt::Byte(reader.read_i8().map_err(|_| NbtError::Eof)?)),
        TagId::Short => Ok(Nbt::Short(reader.read_i16_be().map_err(|_| NbtError::Eof)?)),
        TagId::Int => Ok(Nbt::Int(reader.read_i32_be().map_err(|_| NbtError::Eof)?)),
        TagId::Long => Ok(Nbt::Long(reader.read_i64_be().map_err(|_| NbtError::Eof)?)),
        TagId::Float => Ok(Nbt::Float(reader.read_f32_be().map_err(|_| NbtError::Eof)?)),
        TagId::Double => Ok(Nbt::Double(
            reader.read_f64_be().map_err(|_| NbtError::Eof)?,
        )),
        TagId::ByteArray => {
            let len = usize::try_from(reader.read_i32_be().map_err(|_| NbtError::Eof)?)
                .map_err(|_| NbtError::InvalidCesu8)?;
            let mut values = Vec::with_capacity(len);
            for _ in 0..len {
                values.push(reader.read_i8().map_err(|_| NbtError::Eof)?);
            }
            Ok(Nbt::ByteArray(values.into_boxed_slice()))
        }
        TagId::String => Ok(Nbt::String(read_string(reader)?)),
        TagId::List => {
            let element_tag = read_tag_id(reader)?;
            let len = usize::try_from(reader.read_i32_be().map_err(|_| NbtError::Eof)?)
                .map_err(|_| NbtError::InvalidCesu8)?;
            let mut values = Vec::with_capacity(len);
            for _ in 0..len {
                values.push(read_payload(reader, element_tag, depth + 1)?);
            }
            Ok(Nbt::List(Box::new(NbtList {
                tag: element_tag,
                values,
            })))
        }
        TagId::Compound => Ok(Nbt::Compound(Box::new(read_compound(reader, depth)?))),
        TagId::IntArray => {
            let len = usize::try_from(reader.read_i32_be().map_err(|_| NbtError::Eof)?)
                .map_err(|_| NbtError::InvalidCesu8)?;
            let mut values = Vec::with_capacity(len);
            for _ in 0..len {
                values.push(reader.read_i32_be().map_err(|_| NbtError::Eof)?);
            }
            Ok(Nbt::IntArray(values.into_boxed_slice()))
        }
        TagId::LongArray => {
            let len = usize::try_from(reader.read_i32_be().map_err(|_| NbtError::Eof)?)
                .map_err(|_| NbtError::InvalidCesu8)?;
            let mut values = Vec::with_capacity(len);
            for _ in 0..len {
                values.push(reader.read_i64_be().map_err(|_| NbtError::Eof)?);
            }
            Ok(Nbt::LongArray(values.into_boxed_slice()))
        }
        TagId::End => Ok(Nbt::Byte(0)), // 不会出现；End 由调用方处理
    }
}

fn read_compound(reader: &mut Reader<'_>, depth: usize) -> Result<NbtCompound, NbtError> {
    let mut compound = NbtCompound::new();
    loop {
        let tag = read_tag_id(reader)?;
        if tag == TagId::End {
            return Ok(compound);
        }
        let name = read_string(reader)?;
        let value = read_payload(reader, tag, depth + 1)?;
        compound.insert(name, value);
    }
}

/// 读取命名根标签（Anvil 格式）：标签字节 + 名称 + 载荷。
pub fn decode_named(reader: &mut Reader<'_>) -> Result<Nbt, NbtError> {
    let tag = read_tag_id(reader)?;
    if tag == TagId::End {
        return Ok(Nbt::Compound(Box::new(NbtCompound::new())));
    }
    let _name = read_string(reader)?;
    read_payload(reader, tag, 0)
}

/// 读取未命名根标签（网络格式）；根为 End 时返回 `None`。
pub fn decode_unnamed(reader: &mut Reader<'_>) -> Result<Option<Nbt>, NbtError> {
    let tag = read_tag_id(reader)?;
    if tag == TagId::End {
        return Ok(None);
    }
    read_payload(reader, tag, 0).map(Some)
}

fn write_payload(writer: &mut Writer, value: &Nbt, depth: usize) {
    debug_assert!(depth <= MAX_DEPTH);
    match value {
        Nbt::Byte(v) => writer.write_i8(*v),
        Nbt::Short(v) => writer.write_i16_be(*v),
        Nbt::Int(v) => writer.write_i32_be(*v),
        Nbt::Long(v) => writer.write_i64_be(*v),
        Nbt::Float(v) => writer.write_f32_be(*v),
        Nbt::Double(v) => writer.write_f64_be(*v),
        Nbt::ByteArray(values) => {
            writer.write_i32_be(i32::try_from(values.len()).expect("byte array exceeds i32"));
            for &v in values {
                writer.write_i8(v);
            }
        }
        Nbt::String(s) => write_string(writer, s),
        Nbt::List(list) => {
            let tag = if list.values.is_empty() {
                TagId::End
            } else if list.tag == TagId::End {
                // 防御：构造漏设 tag 时按首元素推导（与 Mojang 语义一致）
                TagId::from_id(tag_id_of(&list.values[0])).expect("known tag id")
            } else {
                list.tag
            };
            writer.write_u8(tag.id());
            writer.write_i32_be(i32::try_from(list.values.len()).expect("list exceeds i32"));
            for v in &list.values {
                write_payload(writer, v, depth + 1);
            }
        }
        Nbt::Compound(compound) => {
            for (key, v) in &compound.entries {
                writer.write_u8(tag_id_of(v));
                write_string(writer, key);
                write_payload(writer, v, depth + 1);
            }
            writer.write_u8(TagId::End.id());
        }
        Nbt::IntArray(values) => {
            writer.write_i32_be(i32::try_from(values.len()).expect("int array exceeds i32"));
            for &v in values {
                writer.write_i32_be(v);
            }
        }
        Nbt::LongArray(values) => {
            writer.write_i32_be(i32::try_from(values.len()).expect("long array exceeds i32"));
            for &v in values {
                writer.write_i64_be(v);
            }
        }
    }
}

const fn tag_id_of(value: &Nbt) -> u8 {
    match value {
        Nbt::Byte(_) => TagId::Byte.id(),
        Nbt::Short(_) => TagId::Short.id(),
        Nbt::Int(_) => TagId::Int.id(),
        Nbt::Long(_) => TagId::Long.id(),
        Nbt::Float(_) => TagId::Float.id(),
        Nbt::Double(_) => TagId::Double.id(),
        Nbt::ByteArray(_) => TagId::ByteArray.id(),
        Nbt::String(_) => TagId::String.id(),
        Nbt::List(_) => TagId::List.id(),
        Nbt::Compound(_) => TagId::Compound.id(),
        Nbt::IntArray(_) => TagId::IntArray.id(),
        Nbt::LongArray(_) => TagId::LongArray.id(),
    }
}

/// 写出命名根标签（Anvil 格式）。
pub fn encode_named(value: &Nbt, writer: &mut Writer) -> Result<(), NbtError> {
    let tag = tag_id_of(value);
    writer.write_u8(tag);
    write_string(writer, "");
    write_payload(writer, value, 0);
    Ok(())
}

/// 写出未命名根标签（网络格式；根复合直接写条目 + End）。
pub fn encode_unnamed(value: &Nbt, writer: &mut Writer) -> Result<(), NbtError> {
    match value {
        Nbt::Compound(compound) => {
            writer.write_u8(TagId::Compound.id());
            for (key, v) in &compound.entries {
                writer.write_u8(tag_id_of(v));
                write_string(writer, key);
                write_payload(writer, v, 1);
            }
            writer.write_u8(TagId::End.id());
            Ok(())
        }
        _ => Err(NbtError::NonCompoundRoot),
    }
}

#[cfg(test)]
mod tests {
    use super::{
        Nbt, NbtCompound, NbtList, TagId, decode_named, decode_unnamed, encode_named,
        encode_unnamed,
    };
    use crate::codec::{Reader, Writer};

    fn compound() -> NbtCompound {
        let mut c = NbtCompound::new();
        c.insert("byte", Nbt::Byte(1));
        c.insert("string", Nbt::String("hello".to_owned()));
        c.insert("long_array", Nbt::LongArray(Box::new([1, 2, 3])));
        let mut inner = NbtCompound::new();
        inner.insert("x", Nbt::Int(42));
        let mut list = NbtList::empty();
        list.tag = TagId::Compound;
        list.values.push(Nbt::Compound(Box::new(inner)));
        c.insert("list", Nbt::List(Box::new(list)));
        c
    }

    #[test]
    fn unnamed_round_trip() {
        let nbt = Nbt::Compound(Box::new(compound()));
        let mut w = Writer::new();
        encode_unnamed(&nbt, &mut w).unwrap();
        let bytes = w.into_bytes();
        let mut r = Reader::new(&bytes);
        let decoded = decode_unnamed(&mut r).unwrap().unwrap();
        assert_eq!(decoded, nbt);
        assert!(r.is_empty());
    }

    #[test]
    fn named_round_trip() {
        let nbt = Nbt::Compound(Box::new(compound()));
        let mut w = Writer::new();
        encode_named(&nbt, &mut w).unwrap();
        let bytes = w.into_bytes();
        let mut r = Reader::new(&bytes);
        let decoded = decode_named(&mut r).unwrap();
        assert_eq!(decoded, nbt);
        assert!(r.is_empty());
    }

    #[test]
    fn empty_root_is_none() {
        let mut w = Writer::new();
        w.write_u8(0);
        let bytes = w.into_bytes();
        let mut r = Reader::new(&bytes);
        assert!(decode_unnamed(&mut r).unwrap().is_none());
    }
}
