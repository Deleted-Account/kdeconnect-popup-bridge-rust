//! 入口：解析参数 -> 载入配置 -> 建立连接 -> 主循环（含断线重连）。

use kdeconnect_bridge::bridge::{Bridge, Settings};
use kdeconnect_bridge::cli::{self, Args};
use kdeconnect_bridge::config::{Config, Rule};
use kdeconnect_bridge::{Error, Result};
use std::time::Duration;

fn main() {
    let args = match Args::parse(std::env::args().skip(1).collect()) {
        Ok(a) => a,
        Err(e) => {
            eprintln!("{e}");
            eprintln!("\n用法见下：\n{}", cli::HELP);
            std::process::exit(2);
        }
    };
    if let Err(e) = run(args) {
        eprintln!("错误: {e}");
        std::process::exit(1);
    }
}

fn run(args: Args) -> Result<()> {
    if args.help {
        print!("{}", cli::HELP);
        return Ok(());
    }

    let cfg_path = Config::default_path();

    // 配置管理类操作：改完即退出
    if args.list_sounds || !args.set_sound.is_empty() {
        let mut cfg = Config::load(&cfg_path)?;
        for item in &args.set_sound {
            apply_set_sound(&mut cfg, item)?;
        }
        cfg.save(&cfg_path)?;
        print!("{}", cfg.display());
        println!("配置文件: {}", cfg_path.display());
        return Ok(());
    }

    let cfg = Config::load(&cfg_path)?;
    print!("{}", cfg.display());

    let settings = Settings {
        all: args.all,
        app_filter: args.app.clone(),
        no_sound: args.no_sound,
        cli_sound: args.sound.clone(),
        interval: Duration::from_secs_f64(args.interval),
        grace: Duration::from_secs_f64(args.grace),
        timeout_s: args.timeout_s,
        verbose: args.verbose,
        notifier: args.notifier.clone(),
        bus: args.bus.clone(),
    };

    let mut bridge = Bridge::new(settings, cfg, args.device.clone())?;

    // --dump：打印当前通知后退出，用于验证协议连通性
    if args.dump {
        return bridge.dump();
    }

    // 主循环：出错时按指数退避重连，最长 30 秒
    let mut backoff = 1u64;
    loop {
        match bridge.run() {
            Ok(()) => break,
            Err(e) => {
                eprintln!("[错误] {e}；{backoff}s 后重连");
                std::thread::sleep(Duration::from_secs(backoff));
                match bridge.reconnect() {
                    Ok(()) => backoff = 1,
                    Err(e2) => {
                        eprintln!("[错误] 重连失败: {e2}");
                        backoff = (backoff * 2).min(30);
                    }
                }
            }
        }
    }
    Ok(())
}

/// 解析 `--set-sound App=路径`
fn apply_set_sound(cfg: &mut Config, item: &str) -> Result<()> {
    let (key, val) = item.split_once('=').ok_or_else(|| {
        Error::Cli(format!("--set-sound 需要 App=路径 形式，收到: {item}"))
    })?;
    let key = key.trim();
    let val = val.trim();
    if key.is_empty() {
        return Err(Error::Cli("--set-sound 的 App 名不能为空".into()));
    }
    if key.eq_ignore_ascii_case("default") {
        cfg.default.sound = if val.is_empty() { None } else { Some(val.to_string()) };
        println!("default -> {}", if val.is_empty() { "(静音)" } else { val });
        return Ok(());
    }
    if val.is_empty() {
        cfg.apps.remove(key);
        println!("已删除规则: {key}");
        return Ok(());
    }
    // 保留该 App 已有的 timeout/urgency/enabled，只更新音效
    let rule = cfg.apps.entry(key.to_string()).or_insert_with(Rule::default);
    rule.sound = Some(val.to_string());
    println!("{key} -> {val}");
    Ok(())
}
