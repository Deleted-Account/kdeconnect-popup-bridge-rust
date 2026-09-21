//! 极简 JSON 解析与生成（std-only）。
//!
//! 只需要读写一个很小的配置文件，因此不引入 serde：
//! - 解析：递归下降，支持 object/array/string/number/bool/null
//! - 生成：2 空格缩进，键顺序保持插入顺序（便于人读和 diff）

use crate::{Error, Result};
use std::fmt;

#[derive(Debug, Clone, PartialEq)]
pub enum Json {
    Null,
    Bool(bool),
    Number(f64),
    String(String),
    Array(Vec<Json>),
    /// 用 Vec 而非 HashMap：保持书写顺序，配置文件读起来更稳定
    Object(Vec<(String, Json)>),
}

impl Json {
    pub fn get(&self, key: &str) -> Option<&Json> {
        match self {
            Json::Object(entries) => entries.iter().find(|(k, _)| k == key).map(|(_, v)| v),
            _ => None,
        }
    }

    pub fn as_str(&self) -> Option<&str> {
        match self {
            Json::String(s) => Some(s),
            _ => None,
        }
    }

    pub fn as_f64(&self) -> Option<f64> {
        match self {
            Json::Number(n) => Some(*n),
            _ => None,
        }
    }

    pub fn as_bool(&self) -> Option<bool> {
        match self {
            Json::Bool(b) => Some(*b),
            _ => None,
        }
    }

    pub fn as_object(&self) -> Option<&Vec<(String, Json)>> {
        match self {
            Json::Object(entries) => Some(entries),
            _ => None,
        }
    }

    pub fn as_array(&self) -> Option<&Vec<Json>> {
        match self {
            Json::Array(items) => Some(items),
            _ => None,
        }
    }

    pub fn object() -> Self {
        Json::Object(Vec::new())
    }

    /// 插入/覆盖一个键（保持原有位置）
    pub fn insert(&mut self, key: impl Into<String>, value: Json) {
        let key = key.into();
        if let Json::Object(entries) = self {
            if let Some(slot) = entries.iter_mut().find(|(k, _)| *k == key) {
                slot.1 = value;
            } else {
                entries.push((key, value));
            }
        }
    }
}

impl fmt::Display for Json {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&stringify(self))
    }
}

/// 解析 JSON 文本
pub fn parse(input: &str) -> Result<Json> {
    let mut p = Parser {
        b: input.as_bytes(),
        i: 0,
    };
    p.ws();
    let v = p.value()?;
    p.ws();
    if p.i != p.b.len() {
        return Err(Error::Config(format!(
            "JSON 结尾有多余内容（位置 {}）",
            p.i
        )));
    }
    Ok(v)
}

struct Parser<'a> {
    b: &'a [u8],
    i: usize,
}

impl<'a> Parser<'a> {
    fn ws(&mut self) {
        while self.i < self.b.len() && matches!(self.b[self.i], b' ' | b'\t' | b'\r' | b'\n') {
            self.i += 1;
        }
    }

    fn peek(&self) -> Result<u8> {
        self.b
            .get(self.i)
            .copied()
            .ok_or_else(|| Error::Config("JSON 意外结束".into()))
    }

    fn expect(&mut self, lit: &str) -> Result<()> {
        if self.b[self.i..].starts_with(lit.as_bytes()) {
            self.i += lit.len();
            Ok(())
        } else {
            Err(Error::Config(format!(
                "期望 {lit}，实际位置 {} 处内容不符",
                self.i
            )))
        }
    }

    fn value(&mut self) -> Result<Json> {
        self.ws();
        match self.peek()? {
            b'{' => self.object(),
            b'[' => self.array(),
            b'"' => Ok(Json::String(self.string()?)),
            b't' => {
                self.expect("true")?;
                Ok(Json::Bool(true))
            }
            b'f' => {
                self.expect("false")?;
                Ok(Json::Bool(false))
            }
            b'n' => {
                self.expect("null")?;
                Ok(Json::Null)
            }
            _ => self.number(),
        }
    }

    fn object(&mut self) -> Result<Json> {
        self.expect("{")?;
        let mut entries = Vec::new();
        self.ws();
        if self.peek()? == b'}' {
            self.i += 1;
            return Ok(Json::Object(entries));
        }
        loop {
            self.ws();
            let key = self.string()?;
            self.ws();
            if self.peek()? != b':' {
                return Err(Error::Config(format!("对象缺少 ':'（位置 {}）", self.i)));
            }
            self.i += 1;
            let val = self.value()?;
            entries.push((key, val));
            self.ws();
            match self.peek()? {
                b',' => self.i += 1,
                b'}' => {
                    self.i += 1;
                    return Ok(Json::Object(entries));
                }
                _ => return Err(Error::Config(format!("对象缺少 ',' 或 '}}'（位置 {}）", self.i))),
            }
        }
    }

    fn array(&mut self) -> Result<Json> {
        self.expect("[")?;
        let mut items = Vec::new();
        self.ws();
        if self.peek()? == b']' {
            self.i += 1;
            return Ok(Json::Array(items));
        }
        loop {
            items.push(self.value()?);
            self.ws();
            match self.peek()? {
                b',' => self.i += 1,
                b']' => {
                    self.i += 1;
                    return Ok(Json::Array(items));
                }
                _ => return Err(Error::Config(format!("数组缺少 ',' 或 ']'（位置 {}）", self.i))),
            }
        }
    }

    fn string(&mut self) -> Result<String> {
        self.expect("\"")?;
        let mut out = String::new();
        loop {
            let c = self.peek()?;
            self.i += 1;
            match c {
                b'"' => return Ok(out),
                b'\\' => {
                    let esc = self.peek()?;
                    self.i += 1;
                    match esc {
                        b'"' => out.push('"'),
                        b'\\' => out.push('\\'),
                        b'/' => out.push('/'),
                        b'b' => out.push('\u{8}'),
                        b'f' => out.push('\u{c}'),
                        b'n' => out.push('\n'),
                        b'r' => out.push('\r'),
                        b't' => out.push('\t'),
                        b'u' => {
                            let code = self.unicode_escape()?;
                            out.push(char::from_u32(code).unwrap_or('\u{fffd}'));
                        }
                        other => {
                            return Err(Error::Config(format!("非法转义 \\{}", other as char)))
                        }
                    }
                }
                _ => {
                    // 逐字节收集 UTF-8（JSON 字符串本身是 UTF-8）
                    let start = self.i - 1;
                    while self.i < self.b.len() && self.b[self.i] != b'"' && self.b[self.i] != b'\\'
                    {
                        self.i += 1;
                    }
                    let chunk = std::str::from_utf8(&self.b[start..self.i])
                        .map_err(|e| Error::Config(format!("字符串不是合法 UTF-8: {e}")))?;
                    out.push_str(chunk);
                }
            }
        }
    }

    fn unicode_escape(&mut self) -> Result<u32> {
        if self.i + 4 > self.b.len() {
            return Err(Error::Config("\\u 转义不完整".into()));
        }
        let hex = std::str::from_utf8(&self.b[self.i..self.i + 4])
            .map_err(|_| Error::Config("\\u 转义非法".into()))?;
        let code = u32::from_str_radix(hex, 16)
            .map_err(|_| Error::Config(format!("\\u{hex} 不是十六进制")))?;
        self.i += 4;
        Ok(code)
    }

    fn number(&mut self) -> Result<Json> {
        let start = self.i;
        while self.i < self.b.len()
            && matches!(self.b[self.i], b'0'..=b'9' | b'-' | b'+' | b'.' | b'e' | b'E')
        {
            self.i += 1;
        }
        let text = std::str::from_utf8(&self.b[start..self.i])
            .map_err(|_| Error::Config("数字非法".into()))?;
        let n: f64 = text
            .parse()
            .map_err(|_| Error::Config(format!("无法解析数字: {text}")))?;
        Ok(Json::Number(n))
    }
}

/// 生成带缩进的 JSON 文本
pub fn stringify(v: &Json) -> String {
    let mut out = String::new();
    write(&mut out, v, 0);
    out
}

fn write(out: &mut String, v: &Json, indent: usize) {
    match v {
        Json::Null => out.push_str("null"),
        Json::Bool(b) => out.push_str(if *b { "true" } else { "false" }),
        Json::Number(n) => out.push_str(&format_number(*n)),
        Json::String(s) => out.push_str(&quote(s)),
        Json::Array(items) => {
            if items.is_empty() {
                out.push_str("[]");
                return;
            }
            out.push_str("[\n");
            for (i, item) in items.iter().enumerate() {
                out.push_str(&"  ".repeat(indent + 1));
                write(out, item, indent + 1);
                if i + 1 < items.len() {
                    out.push(',');
                }
                out.push('\n');
            }
            out.push_str(&"  ".repeat(indent));
            out.push(']');
        }
        Json::Object(entries) => {
            if entries.is_empty() {
                out.push_str("{}");
                return;
            }
            out.push_str("{\n");
            for (i, (k, val)) in entries.iter().enumerate() {
                out.push_str(&"  ".repeat(indent + 1));
                out.push_str(&quote(k));
                out.push_str(": ");
                write(out, val, indent + 1);
                if i + 1 < entries.len() {
                    out.push(',');
                }
                out.push('\n');
            }
            out.push_str(&"  ".repeat(indent));
            out.push('}');
        }
    }
}

fn format_number(n: f64) -> String {
    if n.fract() == 0.0 && n.abs() < 1e15 {
        format!("{}", n as i64)
    } else {
        format!("{}", n)
    }
}

fn quote(s: &str) -> String {
    let mut out = String::with_capacity(s.len() + 2);
    out.push('"');
    for c in s.chars() {
        match c {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            c if (c as u32) < 0x20 => out.push_str(&format!("\\u{:04x}", c as u32)),
            c => out.push(c),
        }
    }
    out.push('"');
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_object() {
        let v = parse(r#"{"apps": {"WeChat": "/a/b.ogg"}, "default": ""}"#).unwrap();
        let apps = v.get("apps").unwrap();
        assert_eq!(apps.get("WeChat").unwrap().as_str(), Some("/a/b.ogg"));
        assert_eq!(v.get("default").unwrap().as_str(), Some(""));
    }

    #[test]
    fn parse_scalars_and_arrays() {
        let v = parse(r#"[1, -2.5, true, false, null, "hi", [], {}]"#).unwrap();
        let items = v.as_array().unwrap();
        assert_eq!(items[0].as_f64(), Some(1.0));
        assert_eq!(items[1].as_f64(), Some(-2.5));
        assert_eq!(items[2].as_bool(), Some(true));
        assert_eq!(items[3].as_bool(), Some(false));
        assert!(matches!(items[4], Json::Null));
        assert_eq!(items[5].as_str(), Some("hi"));
    }

    #[test]
    fn parse_escapes() {
        let v = parse(r#"{"a": "line\nbreak \"q\" é"}"#).unwrap();
        assert_eq!(
            v.get("a").unwrap().as_str(),
            Some("line\nbreak \"q\" é")
        );
    }

    #[test]
    fn reject_invalid() {
        assert!(parse(r#"{"a": }"#).is_err());
        assert!(parse(r#"{"a" 1}"#).is_err());
        assert!(parse(r#"{"a": 1,}"#).is_err());
        assert!(parse("").is_err());
    }

    #[test]
    fn roundtrip() {
        let src = r#"{"apps": {"WeChat": "/x.ogg"}, "default": ""}"#;
        let v = parse(src).unwrap();
        let text = stringify(&v);
        assert_eq!(parse(&text).unwrap(), v);
    }
}
