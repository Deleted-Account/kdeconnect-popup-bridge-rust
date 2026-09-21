//! D-Bus 序列化（marshalling）。
//!
//! 三个最容易写错的地方，这里都严格按规范处理：
//! 1. **对齐**：写每个值前先补齐到该类型的对齐边界（STRING/ARRAY 是 4，STRUCT/DICT_ENTRY 是 8）。
//! 2. **数组长度不含对齐填充**：长度 = 从「填充之后」到「最后一个元素结束」的字节数。
//! 3. **签名不是字符串**：`g` 用 1 字节长度 + NUL，而不是 UINT32 长度。

use super::signature;
use super::value::Value;

/// 字节序_writer：默认小端（`l`），也支持大端（便于测试与兼容性）
pub struct Writer {
    buf: Vec<u8>,
    le: bool,
}

impl Default for Writer {
    fn default() -> Self {
        Self::new()
    }
}

impl Writer {
    pub fn new() -> Self {
        Writer {
            buf: Vec::with_capacity(256),
            le: true,
        }
    }

    /// `le=true` 小端，`le=false` 大端
    pub fn with_endian(le: bool) -> Self {
        Writer {
            buf: Vec::with_capacity(256),
            le,
        }
    }

    pub fn into_inner(self) -> Vec<u8> {
        self.buf
    }

    pub fn len(&self) -> usize {
        self.buf.len()
    }

    pub fn is_empty(&self) -> bool {
        self.buf.is_empty()
    }

    /// 补齐到 n 字节边界
    pub fn align(&mut self, n: usize) {
        while self.buf.len() % n != 0 {
            self.buf.push(0);
        }
    }

    pub fn u8(&mut self, v: u8) {
        self.buf.push(v);
    }

    pub fn u16(&mut self, v: u16) {
        self.align(2);
        self.buf.extend_from_slice(&self.bytes(v));
    }

    pub fn i16(&mut self, v: i16) {
        self.u16(v as u16)
    }

    pub fn u32(&mut self, v: u32) {
        self.align(4);
        self.buf.extend_from_slice(&self.bytes(v));
    }

    pub fn i32(&mut self, v: i32) {
        self.u32(v as u32)
    }

    pub fn u64(&mut self, v: u64) {
        self.align(8);
        self.buf.extend_from_slice(&self.bytes(v));
    }

    pub fn i64(&mut self, v: i64) {
        self.u64(v as u64)
    }

    pub fn f64(&mut self, v: f64) {
        self.u64(v.to_bits())
    }

    /// STRING / OBJECT_PATH：UINT32 长度 + UTF-8 + NUL
    pub fn string(&mut self, s: &str) {
        self.align(4);
        self.u32(s.len() as u32);
        self.buf.extend_from_slice(s.as_bytes());
        self.buf.push(0);
    }

    /// SIGNATURE：BYTE 长度 + 签名字节 + NUL（注意对齐是 1）
    pub fn signature(&mut self, s: &str) {
        debug_assert!(s.len() <= 255, "签名过长");
        self.u8(s.len() as u8);
        self.buf.extend_from_slice(s.as_bytes());
        self.buf.push(0);
    }

    /// VARIANT：先写签名，再按签名写值
    pub fn variant(&mut self, v: &Value) {
        let sig = v.signature();
        self.signature(&sig);
        self.value(v);
    }

    /// 按值自身类型写入
    pub fn value(&mut self, v: &Value) {
        match v {
            Value::Byte(x) => {
                self.align(1);
                self.u8(*x)
            }
            Value::Bool(b) => {
                self.align(4);
                self.u32(*b as u32)
            }
            Value::Int16(x) => self.i16(*x),
            Value::Uint16(x) => self.u16(*x),
            Value::Int32(x) => self.i32(*x),
            Value::Uint32(x) => self.u32(*x),
            Value::Int64(x) => self.i64(*x),
            Value::Uint64(x) => self.u64(*x),
            Value::Double(x) => self.f64(*x),
            Value::String(s) | Value::ObjectPath(s) => self.string(s),
            Value::Signature(s) => self.signature(s),
            Value::Variant(inner) => self.variant(inner),
            Value::Struct(items) => {
                self.align(8);
                for it in items {
                    self.value(it);
                }
            }
            Value::Array(elem, items) => {
                self.align(4);
                let len_pos = self.buf.len();
                self.u32(0); // 占位，稍后回填
                self.align(signature::alignment_of(elem));
                let start = self.buf.len();
                for it in items {
                    self.value(it);
                }
                let len = (self.buf.len() - start) as u32;
                self.patch_u32(len_pos, len);
            }
            Value::Dict(_, _, entries) => {
                self.align(4);
                let len_pos = self.buf.len();
                self.u32(0);
                self.align(8); // 第一个条目按 8 对齐
                let start = self.buf.len();
                for (k, val) in entries {
                    // 每个条目（本质是结构体）都要重新对齐到 8，
                    // 因为上一条目的长度不一定是 8 的倍数
                    self.align(8);
                    self.value(k);
                    self.value(val);
                }
                let len = (self.buf.len() - start) as u32;
                self.patch_u32(len_pos, len);
            }
        }
    }

    fn bytes<const N: usize>(&self, v: impl Endian<N>) -> [u8; N] {
        if self.le {
            v.to_le()
        } else {
            v.to_be()
        }
    }

    fn patch_u32(&mut self, pos: usize, v: u32) {
        let b = if self.le {
            v.to_le_bytes()
        } else {
            v.to_be_bytes()
        };
        self.buf[pos..pos + 4].copy_from_slice(&b);
    }
}

/// 让 u16/u32/u64 共用同一套字节序转换代码
pub trait Endian<const N: usize> {
    fn to_le(self) -> [u8; N];
    fn to_be(self) -> [u8; N];
}

impl Endian<2> for u16 {
    fn to_le(self) -> [u8; 2] {
        self.to_le_bytes()
    }
    fn to_be(self) -> [u8; 2] {
        self.to_be_bytes()
    }
}

impl Endian<4> for u32 {
    fn to_le(self) -> [u8; 4] {
        self.to_le_bytes()
    }
    fn to_be(self) -> [u8; 4] {
        self.to_be_bytes()
    }
}

impl Endian<8> for u64 {
    fn to_le(self) -> [u8; 8] {
        self.to_le_bytes()
    }
    fn to_be(self) -> [u8; 8] {
        self.to_be_bytes()
    }
}
