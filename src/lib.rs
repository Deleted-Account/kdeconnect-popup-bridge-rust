//! # kdeconnect-bridge
//!
//! 用**纯 Rust 标准库**（零第三方 crate）实现的 KDE Connect 通知「补弹」桥接。
//!
//! ## 它解决什么问题
//! 微信等 App 在 Android 侧会**复用同一个通知 id**：连续消息表现为「原地更新同一条通知」，
//! KDE Connect 只更新历史里那条，不再弹窗，甚至不发出 D-Bus 信号。
//! 本程序同时订阅信号 + 轮询比对内容指纹，内容一变就补弹一次，并为每条通知播放指定音效。
//!
//! ## 模块划分
//! ```text
//! dbus::value        D-Bus 类型系统（Value 枚举，含数组/字典的元素签名）
//! dbus::signature    签名解析（a{sv}、(yv) 这类嵌套结构的切分与对齐计算）
//! dbus::marshal      序列化（支持大小端，8 字节对齐规则）
//! dbus::unmarshal    反序列化
//! dbus::message      消息头/体的组装与解析
//! dbus::mod(Conn)    Unix 套接字连接、SASL EXTERNAL 认证、方法调用、信号读取
//! json               极简 JSON 解析/生成（仅够读写配置）
//! config             音效规则（按 App，可后期扩展超时/紧急度/启停）
//! notify             弹窗后端（notify-send / 原生 D-Bus Notify）与播放器后端（paplay）
//! bridge             业务状态机（内容指纹、去重、grace 抑制、弹窗与音效派发）
//! cli                命令行解析
//! ```
//!
//! ## 扩展点（已预留）
//! - `notify::Popup` / `notify::Player` 是 trait，可换后端而不动业务逻辑
//! - `config::Rule` 除 `sound` 外已支持 `timeout` / `urgency` / `enabled`，后续可加 `icon` / `filter`
//! - `dbus::Conn` 是通用的，后续可复用来实现「原生弹窗」「回复通知」等能力

pub mod bridge;
pub mod cli;
pub mod config;
pub mod dbus;
pub mod json;
pub mod notify;

use std::fmt;

/// 全库统一错误类型：对外只暴露一种错误，便于 `?` 串联。
#[derive(Debug)]
pub enum Error {
    /// 套接字 / 文件 / 子进程等 IO 错误
    Io(std::io::Error),
    /// D-Bus 报文不符合规范（签名非法、长度越界等）
    Protocol(String),
    /// SASL 认证失败
    Auth(String),
    /// 总线返回的错误应答（带错误名和消息）
    Dbus { name: String, message: String },
    /// 等待应答超时
    Timeout(String),
    /// 配置文件读写/解析问题
    Config(String),
    /// 命令行参数问题
    Cli(String),
}

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Error::Io(e) => write!(f, "IO 错误: {e}"),
            Error::Protocol(s) => write!(f, "协议错误: {s}"),
            Error::Auth(s) => write!(f, "认证失败: {s}"),
            Error::Dbus { name, message } => write!(f, "D-Bus 错误 {name}: {message}"),
            Error::Timeout(s) => write!(f, "超时: {s}"),
            Error::Config(s) => write!(f, "配置错误: {s}"),
            Error::Cli(s) => write!(f, "参数错误: {s}"),
        }
    }
}

impl std::error::Error for Error {}

impl From<std::io::Error> for Error {
    fn from(e: std::io::Error) -> Self {
        Error::Io(e)
    }
}

/// 全库统一的 Result 别名
pub type Result<T> = std::result::Result<T, Error>;
