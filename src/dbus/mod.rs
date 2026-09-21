//! D-Bus 连接层：Unix 套接字 + SASL EXTERNAL 认证 + 方法调用 + 信号读取。
//!
//! 只用 std：
//! - `std::os::unix::net::UnixStream` 负责传输
//! - 自己实现 SASL `EXTERNAL`（把 uid 十六进制后发送），失败回退 `ANONYMOUS`
//! - 收发都做**缓冲累积**：读到半个报文不会丢，下次继续拼
//! - 方法调用是同步的：读到的信号先缓存进队列，业务逻辑稍后统一取走

pub mod marshal;
pub mod message;
pub mod signature;
pub mod unmarshal;
pub mod value;

use std::collections::VecDeque;
use std::io::{ErrorKind, Read, Write};
use std::os::unix::net::UnixStream;
use std::path::Path;
use std::time::{Duration, Instant};

pub use message::{Message, MessageType};
pub use value::Value;

use crate::{Error, Result};

/// 等待方法应答的超时（本机总线，10 秒足够宽裕）
const CALL_TIMEOUT: Duration = Duration::from_secs(10);
/// 单次 read 的块大小
const READ_CHUNK: usize = 4096;

/// 读取当前进程的有效 uid（从 /proc/self/status，避免引入 libc）
pub fn current_uid() -> Result<u32> {
    let status = std::fs::read_to_string("/proc/self/status").map_err(|e| {
        Error::Protocol(format!("读取 /proc/self/status 失败: {e}"))
    })?;
    for line in status.lines() {
        if let Some(rest) = line.strip_prefix("Uid:") {
            // Uid: real effective saved filesystem —— 取第二个（effective）
            if let Some(field) = rest.split_whitespace().nth(1) {
                return field
                    .parse::<u32>()
                    .map_err(|_| Error::Protocol("Uid 字段无法解析".into()));
            }
        }
    }
    Err(Error::Protocol("/proc/self/status 中找不到 Uid".into()))
}

/// 解析会话总线地址，返回 Unix 套接字路径。
///
/// 支持：
/// - `unix:path=/run/user/1000/bus`
/// - 直接给路径 `/run/user/1000/bus`
///
/// 注意：`unix:abstract=…` 需要构造抽象套接字地址，稳定版 std 没有相应 API，
/// 因此遇到时回退到 `/run/user/<uid>/bus`，并用清晰的错误告知可用 `--bus` 覆盖。
pub fn session_bus_path(bus_override: Option<&str>) -> Result<String> {
    let raw = match bus_override {
        Some(v) => v.to_string(),
        None => std::env::var("DBUS_SESSION_BUS_ADDRESS").map_err(|_| {
            Error::Config("未设置 DBUS_SESSION_BUS_ADDRESS，可用 --bus 指定套接字路径".into())
        })?,
    };

    // 直接给了路径
    if raw.starts_with('/') {
        return Ok(raw);
    }

    for entry in raw.split(';') {
        let entry = entry.trim();
        let Some(rest) = entry.strip_prefix("unix:") else {
            continue;
        };
        let mut path: Option<&str> = None;
        for kv in rest.split(',') {
            if let Some(v) = kv.strip_prefix("path=") {
                path = Some(v);
            }
        }
        if let Some(p) = path {
            return Ok(percent_decode(p));
        }
    }

    // 回退：systemd 的标准位置
    let uid = current_uid()?;
    let fallback = format!("/run/user/{}/bus", uid);
    if Path::new(&fallback).exists() {
        return Ok(fallback);
    }
    Err(Error::Config(format!(
        "地址中没有 unix:path=（可能是 abstract 套接字，稳定版 std 无法连接）；\
         请用 --bus 指定，例如 --bus /run/user/{}/bus",
        uid
    )))
}

fn percent_decode(s: &str) -> String {
    let bytes = s.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'%' && i + 2 < bytes.len() {
            if let (Some(hi), Some(lo)) = (hex_val(bytes[i + 1]), hex_val(bytes[i + 2])) {
                out.push(hi << 4 | lo);
                i += 3;
                continue;
            }
        }
        out.push(bytes[i]);
        i += 1;
    }
    String::from_utf8_lossy(&out).into_owned()
}

fn hex_val(c: u8) -> Option<u8> {
    match c {
        b'0'..=b'9' => Some(c - b'0'),
        b'a'..=b'f' => Some(c - b'a' + 10),
        b'A'..=b'F' => Some(c - b'A' + 10),
        _ => None,
    }
}

/// 一条到会话总线的连接
pub struct Conn {
    stream: UnixStream,
    serial: u32,
    /// 接收缓冲区：累积未解析完的字节
    rbuf: Vec<u8>,
    /// 缓冲区里已消费到的位置
    rpos: usize,
    /// 调用过程中顺带收到的信号
    signals: VecDeque<Message>,
    /// Hello 返回的唯一名
    pub unique_name: String,
}

impl Conn {
    /// 连接 + 认证（不含 Hello）
    pub fn connect(bus_override: Option<&str>) -> Result<Self> {
        let path = session_bus_path(bus_override)?;
        let stream = UnixStream::connect(&path)
            .map_err(|e| Error::Config(format!("连接总线 {} 失败: {e}", path)))?;
        let mut conn = Conn {
            stream,
            serial: 0,
            rbuf: Vec::with_capacity(READ_CHUNK),
            rpos: 0,
            signals: VecDeque::new(),
            unique_name: String::new(),
        };
        conn.handshake()?;
        Ok(conn)
    }

    /// SASL 握手：EXTERNAL（uid 十六进制），失败则 ANONYMOUS
    fn handshake(&mut self) -> Result<()> {
        self.stream
            .set_read_timeout(Some(Duration::from_secs(5)))
            .map_err(Error::Io)?;
        self.stream
            .set_write_timeout(Some(Duration::from_secs(5)))
            .map_err(Error::Io)?;

        let uid = current_uid().unwrap_or(0);
        let hex_uid: String = uid.to_string().bytes().map(|b| format!("{:02x}", b)).collect();

        // 规范允许（且常见实现依赖）在 AUTH 前发一个 NUL 字节，用于触发凭据传递
        self.stream.write_all(&[0u8]).map_err(Error::Io)?;
        self.stream
            .write_all(format!("AUTH EXTERNAL {}\r\n", hex_uid).as_bytes())
            .map_err(Error::Io)?;
        self.stream.flush().map_err(Error::Io)?;

        let reply = self.read_line()?;
        if !reply.starts_with("OK") {
            self.stream
                .write_all(b"AUTH ANONYMOUS\r\n")
                .map_err(Error::Io)?;
            self.stream.flush().map_err(Error::Io)?;
            let reply2 = self.read_line()?;
            if !reply2.starts_with("OK") {
                return Err(Error::Auth(format!("服务端拒绝认证: {reply2}")));
            }
        }

        self.stream.write_all(b"BEGIN\r\n").map_err(Error::Io)?;
        self.stream.flush().map_err(Error::Io)?;
        Ok(())
    }

    fn read_line(&mut self) -> Result<String> {
        let mut out = Vec::with_capacity(64);
        let mut byte = [0u8; 1];
        loop {
            match self.stream.read(&mut byte) {
                Ok(0) => return Err(Error::Auth("认证阶段连接被关闭".into())),
                Ok(_) => {
                    if byte[0] == b'\n' {
                        break;
                    }
                    out.push(byte[0]);
                }
                Err(e) if e.kind() == ErrorKind::WouldBlock || e.kind() == ErrorKind::TimedOut => {
                    return Err(Error::Timeout("等待认证响应超时".into()))
                }
                Err(e) => return Err(Error::Io(e)),
            }
        }
        Ok(String::from_utf8_lossy(&out).trim().to_string())
    }

    /// `Hello`：拿到本连接的唯一名。必须在任何业务调用之前执行
    pub fn hello(&mut self) -> Result<String> {
        let body = self.method_call(
            "org.freedesktop.DBus",
            "/org/freedesktop/DBus",
            "org.freedesktop.DBus",
            "Hello",
            vec![],
        )?;
        match body.into_iter().next() {
            Some(Value::String(name)) => {
                self.unique_name = name.clone();
                Ok(name)
            }
            other => Err(Error::Protocol(format!("Hello 返回异常: {other:?}"))),
        }
    }

    /// 订阅信号，例如 `type='signal',interface='org.kde.kdeconnect.device.notifications'`
    pub fn add_match(&mut self, rule: &str) -> Result<()> {
        self.method_call(
            "org.freedesktop.DBus",
            "/org/freedesktop/DBus",
            "org.freedesktop.DBus",
            "AddMatch",
            vec![Value::String(rule.to_string())],
        )?;
        Ok(())
    }

    /// 通用方法调用，返回应答的消息体
    pub fn method_call(
        &mut self,
        destination: &str,
        path: &str,
        interface: &str,
        member: &str,
        args: Vec<Value>,
    ) -> Result<Vec<Value>> {
        let msg = Message::method_call(destination, path, interface, member, args);
        let serial = self.send(&msg)?;
        let deadline = Instant::now() + CALL_TIMEOUT;

        loop {
            // 先把缓冲区里能解析的都处理掉
            while let Some(m) = self.try_parse()? {
                if m.typ == MessageType::Signal {
                    self.signals.push_back(m);
                    continue;
                }
                if m.reply_serial == Some(serial) {
                    return match m.typ {
                        MessageType::MethodReturn => Ok(m.body),
                        MessageType::Error => Err(Error::Dbus {
                            name: m.error_name.clone().unwrap_or_default(),
                            message: m
                                .body
                                .first()
                                .and_then(|v| v.as_str())
                                .unwrap_or("")
                                .to_string(),
                        }),
                        _ => Err(Error::Protocol("应答类型异常".into())),
                    };
                }
                // 与本调用无关的应答（理论上不会发生）：丢弃
            }

            let now = Instant::now();
            if now >= deadline {
                return Err(Error::Timeout(format!("{}.{} 调用超时", interface, member)));
            }
            if !self.fill(deadline - now)? {
                return Err(Error::Timeout(format!(
                    "{}.{} 等待应答时无数据",
                    interface, member
                )));
            }
        }
    }

    /// `org.freedesktop.DBus.Properties.GetAll` 的便捷封装
    pub fn get_all(
        &mut self,
        destination: &str,
        path: &str,
        interface: &str,
    ) -> Result<Vec<(String, Value)>> {
        let body = self.method_call(
            destination,
            path,
            "org.freedesktop.DBus.Properties",
            "GetAll",
            vec![Value::String(interface.to_string())],
        )?;
        match body.into_iter().next() {
            Some(Value::Dict(_, _, entries)) => {
                let mut out = Vec::with_capacity(entries.len());
                for (k, v) in entries {
                    if let Some(key) = k.as_str() {
                        out.push((key.to_string(), v.unwrap_variant().clone()));
                    }
                }
                Ok(out)
            }
            other => Err(Error::Protocol(format!("GetAll 返回异常: {other:?}"))),
        }
    }

    fn send(&mut self, msg: &Message) -> Result<u32> {
        self.serial += 1;
        let bytes = msg.to_bytes(self.serial)?;
        self.stream
            .set_write_timeout(Some(Duration::from_secs(5)))
            .map_err(Error::Io)?;
        self.stream.write_all(&bytes).map_err(Error::Io)?;
        self.stream.flush().map_err(Error::Io)?;
        Ok(self.serial)
    }

    /// 从套接字读一批字节进缓冲区；超时（无数据）返回 `false`
    fn fill(&mut self, timeout: Duration) -> Result<bool> {
        self.stream.set_read_timeout(Some(timeout)).map_err(Error::Io)?;
        let mut chunk = [0u8; READ_CHUNK];
        match self.stream.read(&mut chunk) {
            Ok(0) => Err(Error::Protocol("总线连接已关闭".into())),
            Ok(n) => {
                self.rbuf.extend_from_slice(&chunk[..n]);
                Ok(true)
            }
            Err(e) if e.kind() == ErrorKind::WouldBlock || e.kind() == ErrorKind::TimedOut => {
                Ok(false)
            }
            Err(e) => Err(Error::Io(e)),
        }
    }

    /// 缓冲区里若有一个完整报文就解析出来
    fn try_parse(&mut self) -> Result<Option<Message>> {
        // 已消费完就重置，避免缓冲区无界增长
        if self.rpos == self.rbuf.len() {
            self.rbuf.clear();
            self.rpos = 0;
        }
        if self.rpos > READ_CHUNK * 4 {
            self.rbuf.drain(..self.rpos);
            self.rpos = 0;
        }
        let avail = &self.rbuf[self.rpos..];
        if avail.is_empty() {
            return Ok(None);
        }
        match Message::parse(avail)? {
            Some((msg, used)) => {
                self.rpos += used;
                Ok(Some(msg))
            }
            None => Ok(None),
        }
    }

    /// 等待下一条信号；`timeout` 内没有就返回 `Ok(None)`
    pub fn next_signal(&mut self, timeout: Duration) -> Result<Option<Message>> {
        if let Some(m) = self.signals.pop_front() {
            return Ok(Some(m));
        }
        let deadline = Instant::now() + timeout;
        loop {
            while let Some(m) = self.try_parse()? {
                if m.typ == MessageType::Signal {
                    return Ok(Some(m));
                }
                self.signals.push_back(m);
            }
            let now = Instant::now();
            if now >= deadline {
                return Ok(None);
            }
            if !self.fill(deadline - now)? {
                return Ok(None);
            }
        }
    }

    /// 取出调用过程中缓存的信号
    pub fn pop_signal(&mut self) -> Option<Message> {
        self.signals.pop_front()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn uid_is_readable() {
        let uid = current_uid().unwrap();
        assert!(uid > 0);
    }

    #[test]
    fn parse_address_variants() {
        assert_eq!(
            session_bus_path(Some("unix:path=/run/user/1000/bus")).unwrap(),
            "/run/user/1000/bus"
        );
        assert_eq!(
            session_bus_path(Some("/tmp/mybus")).unwrap(),
            "/tmp/mybus"
        );
        // 多个候选地址时取第一个 unix:path=
        assert_eq!(
            session_bus_path(Some("unix:path=/a/b,guid=x;unix:abstract=/tmp/c")).unwrap(),
            "/a/b"
        );
    }

    #[test]
    fn percent_decoding() {
        assert_eq!(percent_decode("/run/user/1000/bus"), "/run/user/1000/bus");
        assert_eq!(percent_decode("/tmp/a%20b/bus"), "/tmp/a b/bus");
    }
}
