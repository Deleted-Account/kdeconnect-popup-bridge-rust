//! 业务状态机：决定「什么时候弹、弹什么、放什么声音」。
//!
//! 策略（与 Python 版一致，保证行为可对照）：
//! - **双通道**：优先用 D-Bus 信号（实时），同时轮询兜底（微信连续消息常常不发信号）
//! - **内容指纹**：`(ticker, title, text)` 变了就认为来了新消息
//! - **grace 抑制**：新通知出现后的短时间内，内容若再次变化就不再补弹，
//!   否则「App 先发标题、紧接着补正文」会被当成两条消息，变成两个窗口
//! - **两种接管模式**：
//!   - 默认：首条交给 KDE Connect 弹窗，我们只补音效（它的弹窗不发声）
//!   - `--all`：关掉 KDE Connect 自带弹窗后由我们弹首条，
//!     这样 `text` 为空但正文藏在 `ticker` 里的 App（东方财富等）也能显示全文

use crate::config::Config;
use crate::dbus::{Message, Value};
use crate::dbus::Conn;
use crate::notify::{NativeNotifier, NotifySend, Options, Paplay, Player, Popup, Silent};
use crate::{Error, Result};
use std::collections::{HashMap, HashSet};
use std::process::Command;
use std::time::{Duration, Instant};

pub const KC_DEST: &str = "org.kde.kdeconnect";
pub const NOTIF_IFACE: &str = "org.kde.kdeconnect.device.notifications";
pub const ITEM_IFACE: &str = "org.kde.kdeconnect.device.notifications.notification";
/// 订阅 KDE Connect 通知相关的全部信号（Posted / Updated / Removed）
pub const MATCH_RULE: &str = "type='signal',interface='org.kde.kdeconnect.device.notifications'";

/// 一次状态变化的来源
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Reason {
    /// 通知首次出现（信号或轮询发现的新 id）
    Posted,
    /// 收到 notificationUpdated 信号
    Updated,
    /// 轮询发现内容变了
    PollUpdate,
}

impl Reason {
    pub fn label(self) -> &'static str {
        match self {
            Reason::Posted => "posted",
            Reason::Updated => "updated",
            Reason::PollUpdate => "poll-update",
        }
    }
}

/// 状态机要做出的动作
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Action {
    /// 弹窗 + 音效
    Popup,
    /// 只放音效（KDE Connect 已经弹过窗了）
    SoundOnly,
    /// 什么都不做
    Skip,
}

/// **纯函数**，便于单测：给定来源与「距首次出现的时间」，决定动作
pub fn decide(reason: Reason, all: bool, since_first: Duration, grace: Duration) -> Action {
    match reason {
        // 新通知：默认交给 KDE Connect 弹窗（我们补音效）；--all 时由我们弹
        Reason::Posted => {
            if all {
                Action::Popup
            } else {
                Action::SoundOnly
            }
        }
        // 内容更新：grace 窗口内视为「同一条消息的补全」（很多 App 会先发标题、
        // 隔几百毫秒再补上正文），一律抑制，避免一条消息弹两次。
        // 注意这里不再区分 all：`--all` 模式下首条是我们自己弹的，
        // 紧接着的补全同样要抑制，否则 KDE Connect 关掉后又会变成两个窗口。
        Reason::Updated | Reason::PollUpdate => {
            if since_first < grace {
                Action::Skip
            } else {
                Action::Popup
            }
        }
    }
}

/// 通知内容指纹：三个字段任一变化都算新消息
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct Fingerprint {
    pub ticker: String,
    pub title: String,
    pub text: String,
}

/// 滑动窗口去重：同一内容短时间内重复上报只处理一次
pub struct Dedup {
    map: HashMap<Fingerprint, Instant>,
    window: Duration,
}

impl Dedup {
    pub fn new(window: Duration) -> Self {
        Dedup {
            map: HashMap::with_capacity(64),
            window,
        }
    }

    pub fn allow(&mut self, key: &Fingerprint, now: Instant) -> bool {
        if let Some(last) = self.map.get(key) {
            if now.duration_since(*last) < self.window {
                return false;
            }
        }
        self.map.insert(key.clone(), now);
        // 无界增长防护：超过阈值时清掉过期项
        if self.map.len() > 256 {
            self.map
                .retain(|_, t| now.duration_since(*t) < Duration::from_secs(60));
        }
        true
    }
}

/// 一条通知的属性（GetAll 的结果）
#[derive(Debug, Default, Clone)]
pub struct Props {
    entries: Vec<(String, Value)>,
}

impl Props {
    pub fn from_entries(entries: Vec<(String, Value)>) -> Self {
        Props { entries }
    }

    pub fn get(&self, key: &str) -> Option<&Value> {
        self.entries.iter().find(|(k, _)| k == key).map(|(_, v)| v)
    }

    /// 取字符串，缺失返回 ""
    pub fn str(&self, key: &str) -> &str {
        self.get(key).and_then(Value::as_str).unwrap_or("")
    }

    pub fn bool(&self, key: &str) -> bool {
        self.get(key).and_then(Value::as_bool).unwrap_or(false)
    }

    pub fn app(&self) -> &str {
        self.str("appName")
    }

    /// KDE Connect 的稳定标识；缺失时回退到通知 id
    pub fn internal_id(&self, fallback: &str) -> String {
        let v = self.str("internalId");
        if v.is_empty() {
            fallback.to_string()
        } else {
            v.to_string()
        }
    }

    pub fn icon(&self) -> Option<&str> {
        if !self.bool("hasIcon") {
            return None;
        }
        let p = self.str("iconPath");
        if p.is_empty() {
            None
        } else {
            Some(p)
        }
    }

    pub fn fingerprint(&self) -> Fingerprint {
        Fingerprint {
            ticker: self.str("ticker").to_string(),
            title: self.str("title").to_string(),
            text: self.str("text").to_string(),
        }
    }

    /// 弹窗标题行：优先 `title`，缺失时退回 `ticker`
    pub fn summary(&self) -> &str {
        let t = self.str("title");
        if t.is_empty() {
            self.str("ticker")
        } else {
            t
        }
    }

    /// 弹窗正文。
    ///
    /// 关键点：不少 App（东方财富等资讯/行情类）只填 `title` 和 `ticker`，
    /// 把 `text` 留空。KDE Connect 自带弹窗按 `title + text` 渲染，于是只能显示标题；
    /// 而 `ticker`（手机状态栏那句）往往是 "标题: 正文" 的整体，
    /// 这里把与标题重复的前缀剥掉，正文就回来了。
    pub fn content(&self) -> String {
        let ticker = self.str("ticker");
        let title = self.str("title");
        let text = self.str("text");

        // ticker 去掉 "标题: " 前缀后剩下的部分
        let rest = strip_title_prefix(ticker, title);

        if !text.is_empty() {
            // text 已经是正文；只有当 ticker 里还有 text 没覆盖到的信息时才拼接
            if !rest.is_empty() && !rest.contains(text) && !text.contains(rest) {
                return format!("{rest}\n{text}");
            }
            return text.to_string();
        }
        rest.to_string()
    }
}

/// 把 "标题: 正文" 里的标题前缀剥掉（中文全角冒号同样处理）。
///
/// 标题为空、或 ticker 不是以标题开头时，原样返回 ticker。
pub fn strip_title_prefix<'a>(ticker: &'a str, title: &str) -> &'a str {
    if title.is_empty() {
        return ticker;
    }
    match ticker.strip_prefix(title) {
        Some(rest) => rest
            .strip_prefix(':')
            .or_else(|| rest.strip_prefix('：'))
            .unwrap_or(rest)
            .trim(),
        None => ticker,
    }
}

/// 运行参数（与 CLI 解耦，便于测试）
pub struct Settings {
    /// 连首条通知也由本程序弹（需先关闭 KDE Connect 自带弹窗）
    pub all: bool,
    /// 只处理这些 App；空表示全部
    pub app_filter: Vec<String>,
    pub no_sound: bool,
    /// 命令行临时音效，优先级低于按 App 的规则
    pub cli_sound: Option<String>,
    pub interval: Duration,
    pub grace: Duration,
    pub timeout_s: f64,
    pub verbose: bool,
    /// "cli" 或 "native"
    pub notifier: String,
    pub bus: Option<String>,
}

impl Default for Settings {
    fn default() -> Self {
        Settings {
            all: false,
            app_filter: Vec::new(),
            no_sound: false,
            cli_sound: None,
            interval: Duration::from_secs(1),
            grace: Duration::from_secs_f64(2.0),
            timeout_s: 10.0,
            verbose: false,
            notifier: "cli".to_string(),
            bus: None,
        }
    }
}

pub struct Bridge {
    conn: Conn,
    devices: Vec<String>,
    s: Settings,
    cfg: Config,
    /// internalId -> 上次看到的内容指纹
    state: HashMap<String, Fingerprint>,
    /// internalId -> 首次出现时刻
    first_seen: HashMap<String, Instant>,
    dedup: Dedup,
    /// 首次轮询只做基线，不补弹，避免登录瞬间被历史通知轰炸
    booted: bool,
    popup: Box<dyn Popup>,
    player: Box<dyn Player>,
}

impl Bridge {
    pub fn new(s: Settings, cfg: Config, devices_override: Vec<String>) -> Result<Self> {
        let mut conn = Conn::connect(s.bus.as_deref())?;
        let name = conn.hello()?;
        conn.add_match(MATCH_RULE)?;

        let devices = if devices_override.is_empty() {
            // 未给 --device 时一直等到手机上线；显式指定则直接用，不必等待
            detect_devices_waiting(DetectWait::from_env())?
        } else {
            devices_override
        };

        let popup: Box<dyn Popup> = if s.notifier == "native" {
            match NativeNotifier::new(s.bus.as_deref()) {
                Ok(n) => Box::new(n),
                Err(e) => {
                    eprintln!("[警告] 原生弹窗后端不可用（{e}），回退 notify-send");
                    Box::new(NotifySend)
                }
            }
        } else {
            Box::new(NotifySend)
        };
        let player: Box<dyn Player> = if s.no_sound {
            Box::new(Silent)
        } else {
            Box::new(Paplay)
        };

        println!("已连接会话总线: {name}");
        println!(
            "弹窗后端: {} / 声音后端: {}",
            popup.name(),
            player.name()
        );
        for d in &devices {
            println!("监听设备 {d}（信号 + 每 {:.1}s 轮询）", s.interval.as_secs_f64());
        }

        Ok(Bridge {
            conn,
            devices,
            s,
            cfg,
            state: HashMap::new(),
            first_seen: HashMap::new(),
            dedup: Dedup::new(Duration::from_millis(1500)),
            booted: false,
            popup,
            player,
        })
    }

    /// 断线后重建连接并重置状态（下次轮询重新建立基线）
    pub fn reconnect(&mut self) -> Result<()> {
        let mut conn = Conn::connect(self.s.bus.as_deref())?;
        conn.hello()?;
        conn.add_match(MATCH_RULE)?;
        self.conn = conn;
        self.state.clear();
        self.first_seen.clear();
        self.booted = false;
        println!("已重新连接总线");
        Ok(())
    }

    fn active_ids(&mut self, dev: &str) -> Result<Vec<String>> {
        let path = format!("/modules/kdeconnect/devices/{}/notifications", dev);
        let body = self
            .conn
            .method_call(KC_DEST, &path, NOTIF_IFACE, "activeNotifications", vec![])?;
        match body.into_iter().next() {
            Some(Value::Array(_, items)) => Ok(items
                .into_iter()
                .filter_map(|v| v.as_str().map(str::to_string))
                .collect()),
            other => Err(Error::Protocol(format!("activeNotifications 返回异常: {other:?}"))),
        }
    }

    fn props(&mut self, dev: &str, nid: &str) -> Result<Props> {
        let path = format!("/modules/kdeconnect/devices/{}/notifications/{}", dev, nid);
        let entries = self.conn.get_all(KC_DEST, &path, ITEM_IFACE)?;
        if self.s.verbose {
            println!("[debug] props({nid}) = {:?}", entries);
        }
        Ok(Props::from_entries(entries))
    }

    /// 一次轮询：比对内容指纹，发现变化就派发
    pub fn poll(&mut self) -> Result<()> {
        let devices = self.devices.clone();
        let mut seen: HashSet<String> = HashSet::new();

        for dev in devices {
            let ids = match self.active_ids(&dev) {
                Ok(v) => v,
                Err(e) => {
                    // 设备暂时不可达不应中断整体循环
                    eprintln!("[警告] 读取 {dev} 的通知列表失败: {e}");
                    continue;
                }
            };
            for nid in ids {
                let props = match self.props(&dev, &nid) {
                    Ok(p) => p,
                    Err(e) => {
                        eprintln!("[警告] 读取通知 {nid} 失败: {e}");
                        continue;
                    }
                };
                let internal = props.internal_id(&nid);
                seen.insert(internal.clone());
                let fp = props.fingerprint();

                let is_new = !self.state.contains_key(&internal);
                let changed = !is_new && self.state.get(&internal) != Some(&fp);

                if !self.booted {
                    // 首次轮询：只建立基线
                    self.state.insert(internal, fp);
                    continue;
                }
                if is_new {
                    self.dispatch(&props, Reason::Posted);
                } else if changed {
                    self.dispatch(&props, Reason::PollUpdate);
                }
            }
        }

        // 手机端已清除的通知：忘掉它
        self.state.retain(|k, _| seen.contains(k));
        self.first_seen.retain(|k, _| seen.contains(k));
        self.booted = true;
        Ok(())
    }

    /// 核心派发：更新状态 -> 决策 -> 去重 -> 弹窗/音效
    fn dispatch(&mut self, props: &Props, reason: Reason) {
        let app = props.app();
        let app = if app.is_empty() { "手机通知" } else { app };

        if !self.s.app_filter.is_empty()
            && !self.s.app_filter.iter().any(|a| a == app)
        {
            return;
        }

        let internal = props.internal_id("");
        let fp = props.fingerprint();
        let now = Instant::now();

        self.state.insert(internal.clone(), fp.clone());
        let first = *self.first_seen.entry(internal).or_insert(now);
        let since = now.saturating_duration_since(first);

        let action = decide(reason, self.s.all, since, self.s.grace);
        if self.s.verbose {
            println!(
                "[debug] app={app} reason={:?} since={:?} action={:?}",
                reason, since, action
            );
        }

        if action == Action::Skip {
            println!(
                "[skip] [{app}] 首次弹窗后 {:.1}s 内不补弹",
                self.s.grace.as_secs_f64()
            );
            return;
        }

        if !self.dedup.allow(&fp, now) {
            return;
        }

        let rule = self.cfg.rule_for(app);
        if !rule.enabled {
            println!("[忽略] [{app}] 规则已禁用");
            return;
        }

        if action == Action::SoundOnly {
            println!("[{}] [{app}] 由 KDE Connect 弹窗，仅补音效", reason.label());
            self.play(app, &rule);
            return;
        }

        let summary = props.summary();
        let content = props.content();
        println!("[{}] [{app}] {summary} — {content}", reason.label());
        let opt = Options {
            // 用真实 App 名作为弹窗来源，比统一的「手机通知」更好辨认
            app_name: app,
            timeout_ms: (rule.timeout_s.unwrap_or(self.s.timeout_s) * 1000.0) as i32,
            urgency: rule.urgency.as_deref().unwrap_or("normal"),
            icon: props.icon(),
        };
        if let Err(e) = self.popup.show(summary, &content, opt) {
            eprintln!("[错误] 弹窗失败: {e}");
        }
        self.play(app, &rule);
    }

    fn play(&self, app: &str, rule: &crate::config::Rule) {
        if self.s.no_sound {
            return;
        }
        let sound = rule.sound.clone().or_else(|| self.s.cli_sound.clone());
        match sound {
            Some(p) if !p.is_empty() => {
                if !std::path::Path::new(&p).exists() {
                    eprintln!("[警告] 提示音不存在: {p}");
                    return;
                }
                println!("[音效] [{app}] {p}");
                if let Err(e) = self.player.play(&p) {
                    eprintln!("[错误] 播放失败: {e}");
                }
            }
            _ => println!("[静音] [{app}] 未配置音效"),
        }
    }

    /// 处理一条 D-Bus 信号
    pub fn on_signal(&mut self, msg: &Message) {
        let member = match &msg.member {
            Some(m) => m.clone(),
            None => return,
        };
        if self.s.verbose {
            println!("[signal] {member} path={:?}", msg.path);
        }
        let path = msg.path.clone().unwrap_or_default();
        let dev = match device_from_path(&path) {
            Some(d) => d,
            None => return,
        };

        match member.as_str() {
            "notificationPosted" | "notificationUpdated" => {
                let nid = match msg.body.first().and_then(Value::as_str) {
                    Some(n) => n.to_string(),
                    None => return,
                };
                let reason = if member == "notificationPosted" {
                    Reason::Posted
                } else {
                    Reason::Updated
                };
                match self.props(&dev, &nid) {
                    Ok(props) => self.dispatch(&props, reason),
                    Err(e) => eprintln!("[警告] 读取通知属性失败: {e}"),
                }
            }
            // 手机端一次性清空通知：重置状态，避免残留导致误判
            "allNotificationsRemoved" => {
                self.state.clear();
                self.first_seen.clear();
            }
            _ => {}
        }
    }

    /// 主循环：轮询 + 在间隔内监听信号
    pub fn run(&mut self) -> Result<()> {
        loop {
            self.poll()?;
            let deadline = Instant::now() + self.s.interval;
            loop {
                let now = Instant::now();
                if now >= deadline {
                    break;
                }
                match self.conn.next_signal(deadline - now)? {
                    Some(msg) => self.on_signal(&msg),
                    None => break,
                }
            }
            // 处理调用过程中缓存下来的信号
            while let Some(msg) = self.conn.pop_signal() {
                self.on_signal(&msg);
            }
        }
    }

    /// 调试用：打印当前手机上的所有通知
    pub fn dump(&mut self) -> Result<()> {
        let devices = self.devices.clone();
        for dev in devices {
            let ids = self.active_ids(&dev)?;
            println!("设备 {dev}: {} 条通知", ids.len());
            for nid in ids {
                match self.props(&dev, &nid) {
                    Ok(p) => println!(
                        "  - id={nid} app={} internalId={} ticker={:?} title={:?} text={:?}",
                        p.app(),
                        p.internal_id(""),
                        p.str("ticker"),
                        p.str("title"),
                        p.str("text")
                    ),
                    Err(e) => println!("  - id={nid} 读取失败: {e}"),
                }
            }
        }
        Ok(())
    }
}

/// 从 `/modules/kdeconnect/devices/<id>/notifications/...` 里取出设备 id
pub fn device_from_path(path: &str) -> Option<String> {
    let rest = path.strip_prefix("/modules/kdeconnect/devices/")?;
    let id = rest.split('/').next().unwrap_or("");
    if id.is_empty() {
        None
    } else {
        Some(id.to_string())
    }
}

/// 调用 `kdeconnect-cli` 探测在线设备
pub fn detect_devices() -> Result<Vec<String>> {
    let out = Command::new("kdeconnect-cli")
        .args(["-a", "--id-only"])
        .output()
        .map_err(|e| Error::Config(format!("无法执行 kdeconnect-cli: {e}")))?;
    let text = String::from_utf8_lossy(&out.stdout);
    let ids: Vec<String> = text
        .lines()
        .map(|l| l.trim())
        .filter(|l| !l.is_empty())
        .map(|s| s.to_string())
        .collect();
    if ids.is_empty() {
        return Err(Error::Config(
            "未发现已配对且在线的设备，可用 --device 手动指定".into(),
        ));
    }
    Ok(ids)
}

/// 探测设备的重试间隔
pub const DETECT_INTERVAL: Duration = Duration::from_secs(2);
/// 无限等待时每隔多久汇报一次「还在等」—— 既免得日志刷屏，也便于确认进程没卡死
pub const DETECT_HEARTBEAT: Duration = Duration::from_secs(30);

/// 等手机上线的策略
///
/// 开机时 XDG autostart 与 `kdeconnectd` 是并行启动的，本程序常常先跑起来，
/// 此时守护进程还没完成设备握手、手机也可能还没连上同一个 Wi-Fi，
/// `detect_devices()` 只拿得到空列表。若就此退出，之后手机再连上也没人补弹了 ——
/// 这正是「重启后通知补弹失效」的根因。所以默认是**一直等**，而非给一个时限。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DetectWait {
    /// 一直重试到设备出现（默认）
    Forever,
    /// 只探测一次，失败立即返回错误（脚本/测试要快速失败时用）
    Once,
    /// 最多等这么久
    Limited(Duration),
}

impl DetectWait {
    /// 按环境变量 `KC_BRIDGE_DETECT_WAIT` 决定策略，未设置或无法识别时取 [`Self::Forever`]
    pub fn from_env() -> Self {
        let Ok(raw) = std::env::var("KC_BRIDGE_DETECT_WAIT") else {
            return DetectWait::Forever;
        };
        Self::parse(raw.trim())
    }

    /// 解析 `from_env` 的取值：
    /// `forever`/`always`/`-1`/空 → 一直等；`0`/`never`/`none` → 不等；其余按秒数
    pub fn parse(raw: &str) -> Self {
        match raw.to_ascii_lowercase().as_str() {
            "" | "forever" | "always" | "infinite" | "-1" => DetectWait::Forever,
            "0" | "never" | "none" | "off" => DetectWait::Once,
            other => match other.parse::<u64>() {
                Ok(secs) => DetectWait::Limited(Duration::from_secs(secs)),
                Err(_) => {
                    eprintln!("[警告] KC_BRIDGE_DETECT_WAIT 不是秒数或 forever/never（{raw}），改为一直等待");
                    DetectWait::Forever
                }
            },
        }
    }
}

/// 反复调用 [`detect_devices`]，直到手机上线；策略见 [`DetectWait`]。
pub fn detect_devices_waiting(wait: DetectWait) -> Result<Vec<String>> {
    let mut waited = Duration::ZERO;
    let mut since_log = Duration::ZERO;
    loop {
        let err = match detect_devices() {
            Ok(ids) => {
                if waited > Duration::ZERO {
                    eprintln!("[就绪] 发现设备，共等待 {}s", waited.as_secs());
                }
                return Ok(ids);
            }
            Err(e) => e,
        };
        // 不等，或已到时限 —— 把最后一次探测的错误交还调用方
        if matches!(wait, DetectWait::Once)
            || matches!(wait, DetectWait::Limited(limit) if waited >= limit)
        {
            return Err(err);
        }
        if waited == Duration::ZERO {
            match wait {
                DetectWait::Forever => eprintln!(
                    "[等待] 尚未发现已配对且在线的设备，每 {}s 重试；\
                     会一直等到手机上线（也可用 --device 直接指定）",
                    DETECT_INTERVAL.as_secs()
                ),
                DetectWait::Limited(limit) => eprintln!(
                    "[等待] 尚未发现已配对且在线的设备，每 {}s 重试，最多等待 {}s",
                    DETECT_INTERVAL.as_secs(),
                    limit.as_secs()
                ),
                DetectWait::Once => unreachable!("Once 已在上面返回"),
            }
        } else if waited - since_log >= DETECT_HEARTBEAT {
            eprintln!("[等待] 已等待 {}s，仍在等设备上线…", waited.as_secs());
            since_log = waited;
        }
        std::thread::sleep(DETECT_INTERVAL);
        waited += DETECT_INTERVAL;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn detect_wait_parsing() {
        // 默认与「希望一直等」的写法
        assert_eq!(DetectWait::parse(""), DetectWait::Forever);
        assert_eq!(DetectWait::parse("forever"), DetectWait::Forever);
        assert_eq!(DetectWait::parse("Always"), DetectWait::Forever);
        assert_eq!(DetectWait::parse("-1"), DetectWait::Forever);
        // 不想等的写法
        assert_eq!(DetectWait::parse("0"), DetectWait::Once);
        assert_eq!(DetectWait::parse("never"), DetectWait::Once);
        assert_eq!(DetectWait::parse("None"), DetectWait::Once);
        // 限时
        assert_eq!(
            DetectWait::parse("60"),
            DetectWait::Limited(Duration::from_secs(60))
        );
        // 认不出来的值宁可多等，也不要因为解析失败而错失手机上线
        assert_eq!(DetectWait::parse("abc"), DetectWait::Forever);
    }

    #[test]
    fn posted_without_all_only_sounds() {
        assert_eq!(
            decide(Reason::Posted, false, Duration::ZERO, Duration::from_secs(2)),
            Action::SoundOnly
        );
        assert_eq!(
            decide(Reason::Posted, true, Duration::ZERO, Duration::from_secs(2)),
            Action::Popup
        );
    }

    #[test]
    fn grace_suppresses_first_update() {
        let grace = Duration::from_secs(2);
        assert_eq!(
            decide(Reason::Updated, false, Duration::from_millis(300), grace),
            Action::Skip
        );
        assert_eq!(
            decide(Reason::Updated, false, Duration::from_millis(300), grace),
            Action::Skip
        );
        // 超过 grace 的正常消息要弹
        assert_eq!(
            decide(Reason::PollUpdate, false, Duration::from_secs(3), grace),
            Action::Popup
        );
        // --all（KDE Connect 弹窗已关闭，首条由我们弹）时同样要抑制，
        // 否则 App 补全正文的那一下会变成第二个窗口
        assert_eq!(
            decide(Reason::Updated, true, Duration::ZERO, grace),
            Action::Skip
        );
    }

    #[test]
    fn dedup_window() {
        let mut d = Dedup::new(Duration::from_secs(1));
        let k = Fingerprint {
            ticker: "a".into(),
            title: "b".into(),
            text: "c".into(),
        };
        let t0 = Instant::now();
        assert!(d.allow(&k, t0));
        assert!(!d.allow(&k, t0)); // 窗口内重复
        // 不同内容不受影响
        let k2 = Fingerprint {
            ticker: "x".into(),
            title: "b".into(),
            text: "c".into(),
        };
        assert!(d.allow(&k2, t0));
    }

    #[test]
    fn dedup_zero_window_always_allows() {
        let mut d = Dedup::new(Duration::ZERO);
        let k = Fingerprint {
            ticker: "a".into(),
            title: "".into(),
            text: "".into(),
        };
        let t = Instant::now();
        assert!(d.allow(&k, t));
        assert!(d.allow(&k, t));
    }

    #[test]
    fn device_id_from_path() {
        assert_eq!(
            device_from_path("/modules/kdeconnect/devices/abc123/notifications/0"),
            Some("abc123".into())
        );
        assert_eq!(
            device_from_path("/modules/kdeconnect/devices/abc123/notifications"),
            Some("abc123".into())
        );
        assert_eq!(device_from_path("/other/path"), None);
    }

    #[test]
    fn props_body_and_fingerprint() {
        let p = Props::from_entries(vec![
            ("appName".to_string(), Value::String("WeChat".into())),
            ("ticker".to_string(), Value::String("张三: 在吗".into())),
            ("title".to_string(), Value::String("张三".into())),
            ("text".to_string(), Value::String("在吗".into())),
            (
                "internalId".to_string(),
                Value::String("0|com.tencent.mm|-1|null|1000".into()),
            ),
            ("hasIcon".to_string(), Value::Bool(true)),
            ("iconPath".to_string(), Value::String("/tmp/icon.png".into())),
        ]);
        assert_eq!(p.app(), "WeChat");
        assert_eq!(p.summary(), "张三");
        assert_eq!(p.content(), "在吗"); // ticker 已包含 text，不重复拼接
        assert_eq!(p.icon(), Some("/tmp/icon.png"));
        assert_eq!(p.internal_id("fallback"), "0|com.tencent.mm|-1|null|1000");
        assert_eq!(p.fingerprint().ticker, "张三: 在吗");
    }

    #[test]
    fn props_body_combines_ticker_and_text() {
        let p = Props::from_entries(vec![
            ("ticker".to_string(), Value::String("微信".into())),
            ("text".to_string(), Value::String("你有一条新消息".into())),
        ]);
        assert_eq!(p.content(), "微信\n你有一条新消息");
    }

    /// 东方财富这类 App：`text` 为空，正文只存在于 ticker 的 "标题: 正文" 里。
    /// KDE Connect 自带弹窗看不到它，我们必须把它剥出来。
    #[test]
    fn content_recovered_from_ticker_when_text_empty() {
        let p = Props::from_entries(vec![
            ("appName".to_string(), Value::String("东方财富".into())),
            ("title".to_string(), Value::String("您订阅的资讯提醒".into())),
            ("text".to_string(), Value::String("".into())),
            (
                "ticker".to_string(),
                Value::String("您订阅的资讯提醒: 索宝蛋白 [公告] 索宝蛋白:投资者关系活动记录表".into()),
            ),
        ]);
        assert_eq!(p.summary(), "您订阅的资讯提醒");
        assert_eq!(
            p.content(),
            "索宝蛋白 [公告] 索宝蛋白:投资者关系活动记录表"
        );
    }

    #[test]
    fn strip_title_prefix_handles_fullwidth_colon() {
        assert_eq!(strip_title_prefix("标题：正文", "标题"), "正文");
        assert_eq!(strip_title_prefix("标题: 正文", "标题"), "正文");
        // 不是以标题开头 -> 原样返回
        assert_eq!(strip_title_prefix("别的: 正文", "标题"), "别的: 正文");
        // 标题为空 -> 原样返回
        assert_eq!(strip_title_prefix("正文", ""), "正文");
    }
}
