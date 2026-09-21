//! 弹窗与声音后端。
//!
//! 之所以抽成 trait：业务逻辑（`bridge`）不该关心「怎么弹」。
//! 目前提供两种弹窗实现：
//! - [`NotifySend`]：调用外部 `notify-send`（默认，兼容性最好）
//! - [`NativeNotifier`]：直接用本项目自己的 D-Bus 实现发 `org.freedesktop.Notifications.Notify`
//!   （不需要外部命令，也不经过 shell；这就是自研协议层带来的额外自由度）

use crate::dbus::{Conn, Value};
use crate::{Error, Result};
use std::process::{Command, Stdio};

/// 弹窗参数（未来要加图标主题、分类、动作按钮，都往这里加即可）
pub struct Options<'a> {
    /// notify-send 的 --app-name / Notify 的 app_name
    pub app_name: &'a str,
    /// 停留毫秒数
    pub timeout_ms: i32,
    /// low / normal / critical
    pub urgency: &'a str,
    /// 图标路径或图标名
    pub icon: Option<&'a str>,
}

pub trait Popup {
    fn name(&self) -> &'static str;
    fn show(&mut self, summary: &str, body: &str, opt: Options<'_>) -> Result<()>;
}

/// 默认后端：调用 `notify-send`
pub struct NotifySend;

impl Popup for NotifySend {
    fn name(&self) -> &'static str {
        "notify-send"
    }

    fn show(&mut self, summary: &str, body: &str, opt: Options<'_>) -> Result<()> {
        let mut cmd = Command::new("notify-send");
        cmd.arg("--app-name").arg(opt.app_name);
        cmd.arg("--urgency").arg(opt.urgency);
        cmd.arg("--expire-time").arg(opt.timeout_ms.to_string());
        if let Some(icon) = opt.icon {
            if !icon.is_empty() {
                cmd.arg("--icon").arg(icon);
            }
        }
        cmd.arg(summary).arg(body);
        let status = cmd
            .status()
            .map_err(|e| Error::Config(format!("无法执行 notify-send: {e}")))?;
        if !status.success() {
            return Err(Error::Config(format!(
                "notify-send 退出码 {:?}",
                status.code()
            )));
        }
        Ok(())
    }
}

/// 原生后端：直接发 D-Bus Notify（零外部进程）
pub struct NativeNotifier {
    conn: Conn,
}

impl NativeNotifier {
    pub fn new(bus: Option<&str>) -> Result<Self> {
        let mut conn = Conn::connect(bus)?;
        conn.hello()?;
        Ok(NativeNotifier { conn })
    }
}

impl Popup for NativeNotifier {
    fn name(&self) -> &'static str {
        "native-dbus"
    }

    fn show(&mut self, summary: &str, body: &str, opt: Options<'_>) -> Result<()> {
        // Notify(s app_name, u replaces_id, s app_icon, s summary, s body,
        //        as actions, a{sv} hints, i expire_timeout) -> u
        let args = vec![
            Value::String(opt.app_name.to_string()),
            Value::Uint32(0),
            Value::String(opt.icon.unwrap_or("").to_string()),
            Value::String(summary.to_string()),
            Value::String(body.to_string()),
            Value::Array("s".to_string(), Vec::new()),
            Value::Dict("s".to_string(), "v".to_string(), Vec::new()),
            Value::Int32(opt.timeout_ms),
        ];
        self.conn.method_call(
            "org.freedesktop.Notifications",
            "/org/freedesktop/Notifications",
            "org.freedesktop.Notifications",
            "Notify",
            args,
        )?;
        Ok(())
    }
}

pub trait Player {
    fn name(&self) -> &'static str;
    fn play(&self, path: &str) -> Result<()>;
}

/// 用 `paplay` 播放（走 PipeWire/PulseAudio，支持 wav/ogg/flac/aiff）
pub struct Paplay;

impl Player for Paplay {
    fn name(&self) -> &'static str {
        "paplay"
    }

    fn play(&self, path: &str) -> Result<()> {
        let status = Command::new("paplay")
            .arg(path)
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status()
            .map_err(|e| Error::Config(format!("无法执行 paplay: {e}")))?;
        if !status.success() {
            return Err(Error::Config(format!("paplay 退出码 {:?}", status.code())));
        }
        Ok(())
    }
}

/// 静音后端（`--no-sound`，或未来接入其它播放方式时的占位实现）
pub struct Silent;

impl Player for Silent {
    fn name(&self) -> &'static str {
        "silent"
    }

    fn play(&self, _path: &str) -> Result<()> {
        Ok(())
    }
}
