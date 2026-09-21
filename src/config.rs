//! 音效规则配置（与 Python 版的 `sounds.json` 完全兼容）。
//!
//! 兼容格式：
//! ```json
//! { "apps": { "WeChat": "/path/a.ogg" }, "default": "" }
//! ```
//!
//! 扩展格式（同一份配置可平滑升级，逐 App 细粒度控制）：
//! ```json
//! {
//!   "apps": {
//!     "WeChat": { "sound": "/path/a.ogg", "timeout": 15, "urgency": "critical", "enabled": true }
//!   },
//!   "default": { "sound": "", "timeout": 10 }
//! }
//! ```
//! 两种写法可以混用：值是字符串=只设音效；值是对象=完整规则。

use crate::json::{self, Json};
use crate::{Error, Result};
use std::collections::HashMap;
use std::path::{Path, PathBuf};

/// 单个 App（或 default）的行为规则
#[derive(Debug, Clone, PartialEq)]
pub struct Rule {
    /// 音效文件路径；`None` 表示静音
    pub sound: Option<String>,
    /// 覆盖全局弹窗停留秒数
    pub timeout_s: Option<f64>,
    /// 覆盖紧急度 low/normal/critical
    pub urgency: Option<String>,
    /// false 表示直接忽略该 App
    pub enabled: bool,
}

impl Default for Rule {
    fn default() -> Self {
        Rule {
            sound: None,
            timeout_s: None,
            urgency: None,
            enabled: true,
        }
    }
}

impl Rule {
    fn from_json(v: &Json) -> Result<Rule> {
        match v {
            // 兼容旧写法：直接给路径字符串
            Json::String(s) => {
                let sound = if s.is_empty() { None } else { Some(s.clone()) };
                Ok(Rule {
                    sound,
                    ..Default::default()
                })
            }
            Json::Object(_) => {
                let mut r = Rule::default();
                if let Some(s) = v.get("sound").and_then(Json::as_str) {
                    r.sound = if s.is_empty() { None } else { Some(s.to_string()) };
                }
                if let Some(t) = v.get("timeout").and_then(Json::as_f64) {
                    r.timeout_s = Some(t);
                }
                if let Some(u) = v.get("urgency").and_then(Json::as_str) {
                    r.urgency = Some(u.to_string());
                }
                if let Some(e) = v.get("enabled").and_then(Json::as_bool) {
                    r.enabled = e;
                }
                Ok(r)
            }
            other => Err(Error::Config(format!("规则格式无法识别: {other:?}"))),
        }
    }

    fn to_json(&self) -> Json {
        // 只有音效就用字符串，保持与 Python 版完全一致，方便来回切换
        if self.timeout_s.is_none() && self.urgency.is_none() && self.enabled {
            return Json::String(self.sound.clone().unwrap_or_default());
        }
        let mut obj = Json::object();
        obj.insert("sound", Json::String(self.sound.clone().unwrap_or_default()));
        if let Some(t) = self.timeout_s {
            obj.insert("timeout", Json::Number(t));
        }
        if let Some(u) = &self.urgency {
            obj.insert("urgency", Json::String(u.clone()));
        }
        if !self.enabled {
            obj.insert("enabled", Json::Bool(false));
        }
        obj
    }
}

#[derive(Debug, Default, Clone)]
pub struct Config {
    /// 未命中任何 App 时使用的规则
    pub default: Rule,
    /// 按 App 名（KDE Connect 上报的 appName，如 `WeChat`）索引
    pub apps: HashMap<String, Rule>,
}

impl Config {
    /// 配置文件路径：`KC_BRIDGE_CONFIG` 环境变量优先（便于测试），否则用 XDG 位置
    pub fn default_path() -> PathBuf {
        if let Ok(p) = std::env::var("KC_BRIDGE_CONFIG") {
            return PathBuf::from(p);
        }
        let home = std::env::var("HOME").unwrap_or_else(|_| "/tmp".into());
        PathBuf::from(home)
            .join(".config")
            .join("kdeconnect-popup-bridge")
            .join("sounds.json")
    }

    /// 读取配置；文件不存在或损坏时返回默认配置（静音、无过滤），不阻断启动
    pub fn load(path: &Path) -> Result<Config> {
        let text = match std::fs::read_to_string(path) {
            Ok(t) => t,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(Config::default()),
            Err(e) => return Err(Error::Config(format!("读取 {} 失败: {e}", path.display()))),
        };
        if text.trim().is_empty() {
            return Ok(Config::default());
        }
        let root = json::parse(&text)?;
        let mut cfg = Config::default();
        if let Some(v) = root.get("default") {
            cfg.default = Rule::from_json(v)?;
        }
        if let Some(apps) = root.get("apps").and_then(Json::as_object) {
            for (k, v) in apps {
                cfg.apps.insert(k.clone(), Rule::from_json(v)?);
            }
        }
        Ok(cfg)
    }

    pub fn save(&self, path: &Path) -> Result<()> {
        if let Some(dir) = path.parent() {
            std::fs::create_dir_all(dir)
                .map_err(|e| Error::Config(format!("创建目录 {} 失败: {e}", dir.display())))?;
        }
        let mut root = Json::object();
        let mut apps = Json::object();
        // 排序输出，避免每次保存都产生无意义的 diff
        let mut keys: Vec<&String> = self.apps.keys().collect();
        keys.sort();
        for k in keys {
            apps.insert(k.as_str(), self.apps[k].to_json());
        }
        root.insert("apps", apps);
        root.insert("default", self.default.to_json());
        let text = json::stringify(&root) + "\n";
        std::fs::write(path, text)
            .map_err(|e| Error::Config(format!("写入 {} 失败: {e}", path.display())))
    }

    /// 查找 App 专属规则：先精确匹配，再忽略大小写
    fn lookup(&self, app: &str) -> Option<&Rule> {
        if let Some(r) = self.apps.get(app) {
            return Some(r);
        }
        self.apps
            .iter()
            .find(|(k, _)| k.eq_ignore_ascii_case(app))
            .map(|(_, v)| v)
    }

    /// 合并出该 App 的最终规则：以 default 为底，App 规则逐项覆盖
    pub fn rule_for(&self, app: &str) -> Rule {
        let mut r = self.default.clone();
        if let Some(hit) = self.lookup(app) {
            if hit.sound.is_some() {
                r.sound = hit.sound.clone();
            }
            if hit.timeout_s.is_some() {
                r.timeout_s = hit.timeout_s;
            }
            if hit.urgency.is_some() {
                r.urgency = hit.urgency.clone();
            }
            r.enabled = hit.enabled;
        }
        r
    }

    /// 供 `--list-sounds` 打印
    pub fn display(&self) -> String {
        let mut out = String::from("当前音效规则：\n");
        out.push_str(&format!(
            "  default: {}\n",
            self.default.sound.as_deref().unwrap_or("(静音)")
        ));
        let mut keys: Vec<&String> = self.apps.keys().collect();
        keys.sort();
        for k in keys {
            let r = &self.apps[k];
            out.push_str(&format!(
                "  {}: {}{}\n",
                k,
                r.sound.as_deref().unwrap_or("(静音)"),
                if r.enabled { "" } else { "  [已禁用]" }
            ));
        }
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn legacy_format_compat() {
        let dir = std::env::temp_dir().join(format!("kcbr-{}-legacy", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let p = dir.join("sounds.json");
        std::fs::write(
            &p,
            r#"{"apps": {"WeChat": "/snd/mine.ogg"}, "default": ""}"#,
        )
        .unwrap();
        let cfg = Config::load(&p).unwrap();
        assert_eq!(
            cfg.rule_for("WeChat").sound.as_deref(),
            Some("/snd/mine.ogg")
        );
        assert_eq!(cfg.rule_for("Telegram").sound, None);
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn extended_object_format() {
        let p = std::env::temp_dir().join(format!("kcbr-{}-ext", std::process::id()));
        std::fs::write(
            &p,
            r#"{"apps":{"WeChat":{"sound":"/a.ogg","timeout":15,"urgency":"critical","enabled":false}},"default":{"sound":"/d.ogg","timeout":5}}"#,
        )
        .unwrap();
        let cfg = Config::load(&p).unwrap();
        let r = cfg.rule_for("WeChat");
        assert_eq!(r.sound.as_deref(), Some("/a.ogg"));
        assert_eq!(r.timeout_s, Some(15.0));
        assert_eq!(r.urgency.as_deref(), Some("critical"));
        assert!(!r.enabled);
        let d = cfg.rule_for("Other");
        assert_eq!(d.sound.as_deref(), Some("/d.ogg"));
        assert_eq!(d.timeout_s, Some(5.0));
        std::fs::remove_file(&p).ok();
    }

    #[test]
    fn case_insensitive_lookup() {
        let mut cfg = Config::default();
        cfg.apps.insert(
            "WeChat".into(),
            Rule {
                sound: Some("/a.ogg".into()),
                ..Default::default()
            },
        );
        assert_eq!(cfg.rule_for("wechat").sound.as_deref(), Some("/a.ogg"));
        assert_eq!(cfg.rule_for("WECHAT").sound.as_deref(), Some("/a.ogg"));
    }

    #[test]
    fn save_then_load_roundtrip() {
        let p = std::env::temp_dir().join(format!("kcbr-{}-rt", std::process::id()));
        let mut cfg = Config::default();
        cfg.apps.insert(
            "WeChat".into(),
            Rule {
                sound: Some("/x.ogg".into()),
                ..Default::default()
            },
        );
        cfg.save(&p).unwrap();
        let loaded = Config::load(&p).unwrap();
        assert_eq!(loaded.rule_for("WeChat").sound.as_deref(), Some("/x.ogg"));
        // 只有音效时应写回字符串格式（兼容 Python 版）
        let text = std::fs::read_to_string(&p).unwrap();
        assert!(text.contains("\"WeChat\": \"/x.ogg\""));
        std::fs::remove_file(&p).ok();
    }

    #[test]
    fn missing_file_is_default() {
        let cfg = Config::load(Path::new("/nonexistent/kcbr/sounds.json")).unwrap();
        assert!(cfg.apps.is_empty());
        assert_eq!(cfg.default.sound, None);
    }
}
