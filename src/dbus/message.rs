//! D-Bus 消息的组装与解析。
//!
//! 报文结构（16 字节固定头 + 头字段数组 + 8 字节对齐 + 消息体）：
//! ```text
//! BYTE   字节序 'l' 或 'B'
//! BYTE   消息类型 1=调用 2=返回 3=错误 4=信号
//! BYTE   标志位
//! BYTE   协议版本（固定 1）
//! UINT32 消息体长度
//! UINT32 序列号
//! UINT32 头字段数组长度（不含对齐填充）
//! a(yv)  头字段数组
//! 填充    补齐到 8 字节
//! body   按 signature 序列化的参数
//! ```

use super::marshal::Writer;
use super::signature;
use super::unmarshal::Reader;
use super::value::Value;
use crate::{Error, Result};

pub const METHOD_CALL: u8 = 1;
pub const METHOD_RETURN: u8 = 2;
pub const ERROR: u8 = 3;
pub const SIGNAL: u8 = 4;

// 头字段编号
pub const FIELD_PATH: u8 = 1;
pub const FIELD_INTERFACE: u8 = 2;
pub const FIELD_MEMBER: u8 = 3;
pub const FIELD_ERROR_NAME: u8 = 4;
pub const FIELD_REPLY_SERIAL: u8 = 5;
pub const FIELD_DESTINATION: u8 = 6;
pub const FIELD_SENDER: u8 = 7;
pub const FIELD_SIGNATURE: u8 = 8;
pub const FIELD_UNIX_FDS: u8 = 9;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MessageType {
    MethodCall,
    MethodReturn,
    Error,
    Signal,
}

impl Default for MessageType {
    fn default() -> Self {
        MessageType::MethodCall
    }
}

impl MessageType {
    pub fn to_u8(self) -> u8 {
        match self {
            MessageType::MethodCall => METHOD_CALL,
            MessageType::MethodReturn => METHOD_RETURN,
            MessageType::Error => ERROR,
            MessageType::Signal => SIGNAL,
        }
    }

    pub fn from_u8(v: u8) -> Option<Self> {
        match v {
            METHOD_CALL => Some(MessageType::MethodCall),
            METHOD_RETURN => Some(MessageType::MethodReturn),
            ERROR => Some(MessageType::Error),
            SIGNAL => Some(MessageType::Signal),
            _ => None,
        }
    }
}

#[derive(Debug, Clone, Default)]
pub struct Message {
    pub typ: MessageType,
    /// 本端发出的消息才有意义（解析出来的消息也带）
    pub serial: u32,
    /// 应答/错误对应的调用序列号
    pub reply_serial: Option<u32>,
    pub path: Option<String>,
    pub interface: Option<String>,
    pub member: Option<String>,
    pub destination: Option<String>,
    pub sender: Option<String>,
    pub error_name: Option<String>,
    pub signature: Option<String>,
    pub body: Vec<Value>,
}

impl Message {
    /// 构造一个方法调用
    pub fn method_call(
        destination: &str,
        path: &str,
        interface: &str,
        member: &str,
        body: Vec<Value>,
    ) -> Self {
        Message {
            typ: MessageType::MethodCall,
            destination: Some(destination.to_string()),
            path: Some(path.to_string()),
            interface: Some(interface.to_string()),
            member: Some(member.to_string()),
            body,
            ..Default::default()
        }
    }

    /// 由消息体推导签名（空体则视为无 signature 字段）
    pub fn body_signature(&self) -> String {
        self.body.iter().map(|v| v.signature()).collect()
    }

    /// 序列化：写入 `serial`，返回完整报文字节
    pub fn to_bytes(&self, serial: u32) -> Result<Vec<u8>> {
        // 1) 头字段：必须按编号升序写入（libdbus 的惯例，部分服务端依赖顺序）
        let mut fields: Vec<Value> = Vec::with_capacity(8);
        if let Some(v) = &self.path {
            fields.push(field(FIELD_PATH, Value::ObjectPath(v.clone())));
        }
        if let Some(v) = &self.interface {
            fields.push(field(FIELD_INTERFACE, Value::String(v.clone())));
        }
        if let Some(v) = &self.member {
            fields.push(field(FIELD_MEMBER, Value::String(v.clone())));
        }
        if let Some(v) = &self.error_name {
            fields.push(field(FIELD_ERROR_NAME, Value::String(v.clone())));
        }
        if let Some(v) = self.reply_serial {
            fields.push(field(FIELD_REPLY_SERIAL, Value::Uint32(v)));
        }
        if let Some(v) = &self.destination {
            fields.push(field(FIELD_DESTINATION, Value::String(v.clone())));
        }
        if let Some(v) = &self.sender {
            fields.push(field(FIELD_SENDER, Value::String(v.clone())));
        }
        let sig = self.signature.clone().unwrap_or_else(|| self.body_signature());
        if !sig.is_empty() {
            fields.push(field(FIELD_SIGNATURE, Value::Signature(sig)));
        }
        // 2) 只序列化「元素部分」。
        //    注意：头字段数组紧跟在 16 字节固定头之后，而 16 已是 8 字节对齐，
        //    结构体 (yv) 的对齐要求天然满足，因此这里**不能**插入数组通用规则里的对齐填充。
        let mut hw = Writer::new();
        for f in &fields {
            hw.value(f);
        }
        let elements = hw.into_inner();
        let hdr_len = elements.len() as u32;

        // 3) 再序列化消息体
        let mut bw = Writer::new();
        for v in &self.body {
            bw.value(v);
        }
        let body = bw.into_inner();

        // 4) 拼装
        let mut out = Vec::with_capacity(16 + elements.len() + body.len() + 8);
        out.push(b'l'); // 小端
        out.push(self.typ.to_u8());
        out.push(0); // 标志位：需要应答
        out.push(1); // 协议版本
        out.extend_from_slice(&(body.len() as u32).to_le_bytes());
        out.extend_from_slice(&serial.to_le_bytes());
        out.extend_from_slice(&hdr_len.to_le_bytes());
        out.extend_from_slice(&elements);
        while out.len() % 8 != 0 {
            out.push(0); // 消息体按 8 字节对齐
        }
        out.extend_from_slice(&body);
        Ok(out)
    }

    /// 解析：数据不足返回 `Ok(None)`，调用方继续收字节；错误返回 `Err`
    pub fn parse(buf: &[u8]) -> Result<Option<(Message, usize)>> {
        if buf.len() < 16 {
            return Ok(None);
        }
        let le = match buf[0] {
            b'l' => true,
            b'B' => false,
            other => {
                return Err(Error::Protocol(format!(
                    "未知字节序标记 {:?}",
                    other as char
                )))
            }
        };
        let typ = MessageType::from_u8(buf[1])
            .ok_or_else(|| Error::Protocol(format!("未知消息类型 {}", buf[1])))?;
        // buf[2] 标志位暂不使用
        if buf[3] != 1 {
            return Err(Error::Protocol(format!("不支持的协议版本 {}", buf[3])));
        }

        let mut r = Reader::new(buf, le);
        r.set_pos(4);
        let body_len = r.u32()? as usize;
        let serial = r.u32()?;
        let arr_len = r.u32()? as usize;

        const MAX_HEADER: usize = 1 << 20; // 1MB，防止恶意长度导致巨量分配
        if arr_len > MAX_HEADER || body_len > (1 << 26) {
            return Err(Error::Protocol("报文长度异常".into()));
        }

        let hdr_start = 16usize;
        let hdr_end = hdr_start + arr_len;
        if buf.len() < hdr_end {
            return Ok(None);
        }

        // 头字段数组：这段区间里只有元素（长度字段在固定头里），逐个读到耗尽为止
        let mut hr = Reader::new(&buf[hdr_start..hdr_end], le);
        let mut items: Vec<Value> = Vec::new();
        while hr.remaining() > 0 {
            items.push(hr.value("(yv)")?);
        }

        let mut msg = Message {
            typ,
            serial,
            ..Default::default()
        };
        for item in items {
            let parts = match item {
                Value::Struct(parts) => parts,
                _ => continue,
            };
            if parts.len() != 2 {
                continue;
            }
            let code = match parts[0].as_byte() {
                Some(c) => c,
                None => continue,
            };
            let val = parts[1].unwrap_variant();
            match code {
                FIELD_PATH => msg.path = val.as_str().map(str::to_string),
                FIELD_INTERFACE => msg.interface = val.as_str().map(str::to_string),
                FIELD_MEMBER => msg.member = val.as_str().map(str::to_string),
                FIELD_ERROR_NAME => msg.error_name = val.as_str().map(str::to_string),
                FIELD_REPLY_SERIAL => msg.reply_serial = val.as_u32(),
                FIELD_DESTINATION => msg.destination = val.as_str().map(str::to_string),
                FIELD_SENDER => msg.sender = val.as_str().map(str::to_string),
                FIELD_SIGNATURE => msg.signature = val.as_str().map(str::to_string),
                FIELD_UNIX_FDS => {}
                _ => {}
            }
        }

        // 消息体
        let body_start = align_up(hdr_end, 8);
        let body_end = body_start + body_len;
        if buf.len() < body_end {
            return Ok(None);
        }
        if let Some(sig) = &msg.signature.clone() {
            if !sig.is_empty() {
                let mut br = Reader::new(&buf[body_start..body_end], le);
                let mut rest = sig.as_str();
                while !rest.is_empty() {
                    let (one, tail) = signature::read_one(rest)?;
                    msg.body.push(br.value(one)?);
                    rest = tail;
                }
            }
        }
        Ok(Some((msg, body_end)))
    }
}

/// 头字段：`(BYTE 编号, VARIANT 值)`
fn field(code: u8, v: Value) -> Value {
    Value::Struct(vec![Value::Byte(code), Value::Variant(Box::new(v))])
}

fn align_up(v: usize, n: usize) -> usize {
    (v + n - 1) / n * n
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::dbus::value::{vstr, vvar};

    #[test]
    fn method_call_roundtrip() {
        let m = Message::method_call(
            "org.freedesktop.DBus",
            "/org/freedesktop/DBus",
            "org.freedesktop.DBus",
            "Hello",
            vec![],
        );
        let bytes = m.to_bytes(1).unwrap();
        assert_eq!(bytes[0], b'l');
        assert_eq!(bytes[1], METHOD_CALL);
        assert_eq!(bytes[3], 1);
        assert_eq!(&bytes[8..12], &1u32.to_le_bytes());
        assert_eq!(bytes.len() % 8, 0);

        let (parsed, used) = Message::parse(&bytes).unwrap().unwrap();
        assert_eq!(used, bytes.len());
        assert_eq!(parsed.typ, MessageType::MethodCall);
        assert_eq!(parsed.destination.as_deref(), Some("org.freedesktop.DBus"));
        assert_eq!(parsed.path.as_deref(), Some("/org/freedesktop/DBus"));
        assert_eq!(parsed.member.as_deref(), Some("Hello"));
        assert!(parsed.body.is_empty());
    }

    #[test]
    fn method_call_with_body_roundtrip() {
        let m = Message::method_call("dest", "/p", "iface", "Do", vec![vstr("hello"), Value::Bool(true)]);
        let bytes = m.to_bytes(42).unwrap();
        let (parsed, _) = Message::parse(&bytes).unwrap().unwrap();
        assert_eq!(parsed.serial, 42);
        assert_eq!(parsed.signature.as_deref(), Some("sb"));
        assert_eq!(parsed.body.len(), 2);
        assert_eq!(parsed.body[0].as_str(), Some("hello"));
        assert_eq!(parsed.body[1].as_bool(), Some(true));
    }

    #[test]
    fn dict_body_roundtrip() {
        let hints = Value::Dict(
            "s".into(),
            "v".into(),
            vec![(Value::String("urgency".into()), vvar(Value::Byte(1)))],
        );
        let m = Message::method_call("d", "/p", "i", "M", vec![hints.clone()]);
        let bytes = m.to_bytes(3).unwrap();
        let (parsed, _) = Message::parse(&bytes).unwrap().unwrap();
        assert_eq!(parsed.signature.as_deref(), Some("a{sv}"));
        assert_eq!(parsed.body[0], hints);
    }

    #[test]
    fn incomplete_input_returns_none() {
        let m = Message::method_call("d", "/p", "i", "M", vec![vstr("x")]);
        let bytes = m.to_bytes(1).unwrap();
        assert!(Message::parse(&bytes[..10]).unwrap().is_none());
    }
}
