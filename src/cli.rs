//! 命令行解析（手写，不引入 clap，保持零依赖）。
//!
//! 同时支持 `--flag value` 与 `--flag=value` 两种写法。

use crate::{Error, Result};

pub const HELP: &str = "\
kdeconnect-popup-bridge —— 让手机通知的每一条都在 KDE 桌面弹窗（纯 Rust / 纯 std 实现）

用法:
  kdeconnect-popup-bridge [选项]

运行:
      --all                 连首条通知也由本程序弹（需先关闭 KDE Connect 自带弹窗）
      --app <名称>          只处理指定 App，可重复传参，例如 --app WeChat
      --device <设备ID>     指定设备，可重复；默认自动探测所有在线设备
      --interval <秒>       轮询间隔，默认 1.0
      --grace <秒>          首条弹窗后 N 秒内不补弹，默认 2.0（避免第一条弹两次）
      --timeout <秒>        弹窗停留秒数，默认 10
      --notifier <后端>     cli（默认，调用 notify-send）或 native（直接发 D-Bus Notify）
      --bus <地址>          覆盖 DBUS_SESSION_BUS_ADDRESS，例如 /run/user/1000/bus
      --verbose             打印调试信息（含 D-Bus 属性原文）
  -h, --help               显示本帮助

声音:
      --no-sound            全部静音
      --sound <文件>        临时音效（优先级低于按 App 的规则）
      --set-sound <App=路径> 写入音效规则，可重复；default= 表示其他 App 静音；
                            App= 表示删除该规则。例：--set-sound WeChat=/a/b.ogg
      --list-sounds         打印当前规则后退出

调试:
      --dump                打印手机上当前的通知后退出（验证协议连通性用）

配置文件:
  ~/.config/kdeconnect-popup-bridge/sounds.json（可用环境变量 KC_BRIDGE_CONFIG 覆盖）
  兼容字符串写法 { \"apps\": { \"WeChat\": \"/a.ogg\" }, \"default\": \"\" }，
  也支持对象写法 { \"sound\": \"...\", \"timeout\": 15, \"urgency\": \"critical\", \"enabled\": false }。
";

#[derive(Debug, Clone)]
pub struct Args {
    pub help: bool,
    pub all: bool,
    pub app: Vec<String>,
    pub no_sound: bool,
    pub sound: Option<String>,
    pub set_sound: Vec<String>,
    pub list_sounds: bool,
    pub interval: f64,
    pub timeout_s: f64,
    pub grace: f64,
    pub device: Vec<String>,
    pub dump: bool,
    pub notifier: String,
    pub bus: Option<String>,
    pub verbose: bool,
}

impl Default for Args {
    fn default() -> Self {
        Args {
            help: false,
            all: false,
            app: Vec::new(),
            no_sound: false,
            sound: None,
            set_sound: Vec::new(),
            list_sounds: false,
            interval: 1.0,
            timeout_s: 10.0,
            grace: 2.0,
            device: Vec::new(),
            dump: false,
            notifier: "cli".to_string(),
            bus: None,
            verbose: false,
        }
    }
}

impl Args {
    /// 解析参数；出错时返回带说明的 `Error::Cli`
    pub fn parse(raw: Vec<String>) -> Result<Self> {
        let mut a = Args::default();
        let mut pending: Option<String> = None;
        let mut i = 0usize;

        // 取当前参数的取值：优先用 `--k=v` 里的 v，否则取下一个参数
        macro_rules! value {
            ($name:expr) => {{
                if let Some(v) = pending.take() {
                    v
                } else {
                    if i >= raw.len() {
                        return Err(Error::Cli(format!("{} 缺少取值", $name)));
                    }
                    let v = raw[i].clone();
                    i += 1;
                    v
                }
            }};
        }
        macro_rules! num {
            ($name:expr, $slot:expr) => {{
                let v = value!($name);
                let n: f64 = v
                    .parse()
                    .map_err(|_| Error::Cli(format!("{} 不是数字: {}", $name, v)))?;
                $slot = n;
            }};
        }

        while i < raw.len() {
            let mut arg = raw[i].clone();
            i += 1;
            if arg.starts_with("--") && arg.contains('=') {
                let (k, v) = {
                    let (k, v) = arg.split_once('=').unwrap();
                    (k.to_string(), v.to_string())
                };
                arg = k;
                pending = Some(v);
            }
            match arg.as_str() {
                "-h" | "--help" => a.help = true,
                "--all" => a.all = true,
                "--no-sound" => a.no_sound = true,
                "--list-sounds" => a.list_sounds = true,
                "--dump" => a.dump = true,
                "--verbose" => a.verbose = true,
                "--app" => a.app.push(value!("--app")),
                "--set-sound" => a.set_sound.push(value!("--set-sound")),
                "--device" => a.device.push(value!("--device")),
                "--sound" => a.sound = Some(value!("--sound")),
                "--notifier" => a.notifier = value!("--notifier"),
                "--bus" => a.bus = Some(value!("--bus")),
                "--timeout" => num!("--timeout", a.timeout_s),
                "--interval" => num!("--interval", a.interval),
                "--grace" => num!("--grace", a.grace),
                other if other.starts_with('-') => {
                    return Err(Error::Cli(format!("未知参数 {}", other)))
                }
                other => return Err(Error::Cli(format!("意外的位置参数 {}", other))),
            }
        }

        if a.interval <= 0.0 {
            return Err(Error::Cli("--interval 必须大于 0".into()));
        }
        if a.grace < 0.0 {
            return Err(Error::Cli("--grace 不能为负".into()));
        }
        if a.notifier != "cli" && a.notifier != "native" {
            return Err(Error::Cli("--notifier 只能是 cli 或 native".into()));
        }
        Ok(a)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn parse(args: &[&str]) -> Result<Args> {
        Args::parse(args.iter().map(|s| s.to_string()).collect())
    }

    #[test]
    fn defaults() {
        let a = parse(&[]).unwrap();
        assert_eq!(a.interval, 1.0);
        assert_eq!(a.grace, 2.0);
        assert_eq!(a.timeout_s, 10.0);
        assert_eq!(a.notifier, "cli");
        assert!(!a.all);
    }

    #[test]
    fn space_and_equals_forms() {
        let a = parse(&["--app", "WeChat", "--interval=2.5", "--all"]).unwrap();
        assert_eq!(a.app, vec!["WeChat".to_string()]);
        assert_eq!(a.interval, 2.5);
        assert!(a.all);
    }

    #[test]
    fn repeated_flags() {
        let a = parse(&[
            "--app",
            "WeChat",
            "--app",
            "Telegram",
            "--set-sound",
            "WeChat=/a.ogg",
            "--set-sound",
            "default=",
        ])
        .unwrap();
        assert_eq!(a.app.len(), 2);
        assert_eq!(a.set_sound.len(), 2);
    }

    #[test]
    fn validation() {
        assert!(parse(&["--interval", "0"]).is_err());
        assert!(parse(&["--notifier", "x"]).is_err());
        assert!(parse(&["--app"]).is_err()); // 缺值
        assert!(parse(&["--bogus"]).is_err());
        assert!(parse(&["positional"]).is_err());
    }
}
