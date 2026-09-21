//! D-Bus 反序列化（unmarshalling）。
//!
//! 与序列化严格对称；所有越界访问都返回 `Error::Protocol` 而不是 panic，
//! 因为网络/总线上的字节流是不可信输入。

use super::signature;
use super::value::Value;
use crate::{Error, Result};

/// 只读游标：不拷贝数据，只推进 `pos`
pub struct Reader<'a> {
    buf: &'a [u8],
    pos: usize,
    le: bool,
}

impl<'a> Reader<'a> {
    pub fn new(buf: &'a [u8], le: bool) -> Self {
        Reader { buf, pos: 0, le }
    }

    pub fn pos(&self) -> usize {
        self.pos
    }

    pub fn set_pos(&mut self, pos: usize) {
        self.pos = pos;
    }

    pub fn remaining(&self) -> usize {
        self.buf.len().saturating_sub(self.pos)
    }

    pub fn align(&mut self, n: usize) {
        while self.pos % n != 0 {
            self.pos += 1;
        }
    }

    fn need(&self, n: usize) -> Result<()> {
        if self.pos + n > self.buf.len() {
            Err(Error::Protocol(format!(
                "数据不足：需要 {} 字节，只剩 {} 字节",
                n,
                self.remaining()
            )))
        } else {
            Ok(())
        }
    }

    fn take<const N: usize>(&mut self) -> Result<[u8; N]> {
        self.need(N)?;
        let mut a = [0u8; N];
        a.copy_from_slice(&self.buf[self.pos..self.pos + N]);
        self.pos += N;
        Ok(a)
    }

    pub fn u8(&mut self) -> Result<u8> {
        Ok(self.take::<1>()?[0])
    }

    pub fn u16(&mut self) -> Result<u16> {
        self.align(2);
        let b = self.take::<2>()?;
        Ok(if self.le {
            u16::from_le_bytes(b)
        } else {
            u16::from_be_bytes(b)
        })
    }

    pub fn i16(&mut self) -> Result<i16> {
        Ok(self.u16()? as i16)
    }

    pub fn u32(&mut self) -> Result<u32> {
        self.align(4);
        let b = self.take::<4>()?;
        Ok(if self.le {
            u32::from_le_bytes(b)
        } else {
            u32::from_be_bytes(b)
        })
    }

    pub fn i32(&mut self) -> Result<i32> {
        Ok(self.u32()? as i32)
    }

    pub fn u64(&mut self) -> Result<u64> {
        self.align(8);
        let b = self.take::<8>()?;
        Ok(if self.le {
            u64::from_le_bytes(b)
        } else {
            u64::from_be_bytes(b)
        })
    }

    pub fn i64(&mut self) -> Result<i64> {
        Ok(self.u64()? as i64)
    }

    pub fn f64(&mut self) -> Result<f64> {
        Ok(f64::from_bits(self.u64()?))
    }

    /// STRING / OBJECT_PATH
    pub fn string(&mut self) -> Result<String> {
        self.align(4);
        let len = self.u32()? as usize;
        self.need(len + 1)?; // 含结尾 NUL
        let bytes = &self.buf[self.pos..self.pos + len];
        let s = std::str::from_utf8(bytes)
            .map_err(|e| Error::Protocol(format!("字符串不是合法 UTF-8: {e}")))?
            .to_string();
        self.pos += len + 1; // 跳过 NUL
        Ok(s)
    }

    /// SIGNATURE
    pub fn signature(&mut self) -> Result<String> {
        let len = self.u8()? as usize;
        self.need(len + 1)?;
        let bytes = &self.buf[self.pos..self.pos + len];
        let s = std::str::from_utf8(bytes)
            .map_err(|e| Error::Protocol(format!("签名不是合法 UTF-8: {e}")))?
            .to_string();
        self.pos += len + 1;
        Ok(s)
    }

    /// 按给定签名解析一个值
    pub fn value(&mut self, sig: &str) -> Result<Value> {
        let (t, _rest) = signature::read_one(sig)?;
        self.value_of(t)
    }

    fn value_of(&mut self, t: &str) -> Result<Value> {
        let first = t.as_bytes().first().copied().unwrap_or(0);
        match first {
            b'y' => {
                self.align(1);
                Ok(Value::Byte(self.u8()?))
            }
            b'b' => {
                self.align(4);
                Ok(Value::Bool(self.u32()? != 0))
            }
            b'n' => Ok(Value::Int16(self.i16()?)),
            b'q' => Ok(Value::Uint16(self.u16()?)),
            b'i' => Ok(Value::Int32(self.i32()?)),
            b'u' => Ok(Value::Uint32(self.u32()?)),
            b'x' => Ok(Value::Int64(self.i64()?)),
            b't' => Ok(Value::Uint64(self.u64()?)),
            b'd' => Ok(Value::Double(self.f64()?)),
            b's' => Ok(Value::String(self.string()?)),
            b'o' => Ok(Value::ObjectPath(self.string()?)),
            b'g' => Ok(Value::Signature(self.signature()?)),
            b'v' => {
                let inner_sig = self.signature()?;
                Ok(Value::Variant(Box::new(self.value(&inner_sig)?)))
            }
            b'a' => self.array_of(t),
            b'(' => {
                self.align(8);
                let fields = signature::struct_fields(t)?;
                let mut items = Vec::with_capacity(fields.len());
                for f in fields {
                    items.push(self.value_of(f)?);
                }
                Ok(Value::Struct(items))
            }
            other => Err(Error::Protocol(format!(
                "不支持的类型 '{}'",
                other as char
            ))),
        }
    }

    fn array_of(&mut self, t: &str) -> Result<Value> {
        self.align(4);
        let len = self.u32()? as usize;
        let b = t.as_bytes();

        if b.len() > 1 && b[1] == b'{' {
            // 字典：a{kv}
            let (key_sig, val_sig) = signature::dict_types(t)?;
            self.align(8);
            let end = self.pos + len;
            if end > self.buf.len() {
                return Err(Error::Protocol("字典长度超出缓冲区".into()));
            }
            let mut entries = Vec::new();
            while self.pos < end {
                self.align(8);
                let k = self.value_of(key_sig)?;
                let v = self.value_of(val_sig)?;
                entries.push((k, v));
            }
            if self.pos != end {
                return Err(Error::Protocol("字典条目长度与声明不符".into()));
            }
            Ok(Value::Dict(key_sig.to_string(), val_sig.to_string(), entries))
        } else {
            let elem = signature::array_elem(t)?;
            self.align(signature::alignment_of(elem));
            let end = self.pos + len;
            if end > self.buf.len() {
                return Err(Error::Protocol("数组长度超出缓冲区".into()));
            }
            let mut items = Vec::new();
            while self.pos < end {
                items.push(self.value_of(elem)?);
            }
            if self.pos != end {
                return Err(Error::Protocol("数组元素长度与声明不符".into()));
            }
            Ok(Value::Array(elem.to_string(), items))
        }
    }
}
