//! D-Bus 签名（类型字符串）的解析与对齐计算。
//!
//! 签名是可以嵌套的：`a{sv}`、`(yv)`、`aa{si}`。
//! 序列化/反序列化都需要「从签名里切出一个完整类型」的能力，因此这里提供一个
//! 递归下降的扫描器 `read_one`，以及若干常用辅助函数。
//!
//! 对齐规则（D-Bus 规范）：
//! ```text
//! y=1  g=1  v=1  n/q=2  b/i/u/s/o=4  x/t/d=8  数组(a…)=4  结构体/字典条目=8
//! ```

use crate::{Error, Result};

/// 是否为「单个字符就是一个完整类型」的类型
pub fn is_single(c: u8) -> bool {
    matches!(
        c,
        b'y' | b'b'
            | b'n'
            | b'q'
            | b'i'
            | b'u'
            | b'x'
            | b't'
            | b'd'
            | b's'
            | b'o'
            | b'g'
            | b'v'
    )
}

/// 给定（单个完整类型的）签名首字符，返回其对齐字节数
pub fn alignment_of(sig: &str) -> usize {
    match sig.as_bytes().first() {
        Some(b'y') | Some(b'g') | Some(b'v') => 1,
        Some(b'n') | Some(b'q') => 2,
        Some(b'b') | Some(b'i') | Some(b'u') | Some(b's') | Some(b'o') | Some(b'a') => 4,
        Some(b'x') | Some(b't') | Some(b'd') => 8,
        Some(b'(') | Some(b'{') => 8,
        _ => 1,
    }
}

/// 从签名开头切出**一个完整类型**，返回 `(该类型, 剩余部分)`。
///
/// ```text
/// read_one("a{sv}as") -> ("a{sv}", "as")
/// read_one("(yv)s")   -> ("(yv)", "s")
/// ```
pub fn read_one(s: &str) -> Result<(&str, &str)> {
    let bytes = s.as_bytes();
    let mut i = 0usize;
    scan(bytes, &mut i)?;
    Ok((&s[..i], &s[i..]))
}

/// 把整条签名切成若干完整类型
pub fn split_all(s: &str) -> Result<Vec<&str>> {
    let mut rest = s;
    let mut out = Vec::new();
    while !rest.is_empty() {
        let (one, tail) = read_one(rest)?;
        out.push(one);
        rest = tail;
    }
    Ok(out)
}

/// `a{sv}` -> `("s", "v")`
pub fn dict_types(t: &str) -> Result<(&str, &str)> {
    let b = t.as_bytes();
    if b.len() < 4 || b[0] != b'a' || b[1] != b'{' || *b.last().unwrap() != b'}' {
        return Err(Error::Protocol(format!("不是字典签名: {t}")));
    }
    let inner = &t[2..t.len() - 1];
    let (key, rest) = read_one(inner)?;
    if !is_basic_first(key) {
        return Err(Error::Protocol(format!("字典键必须是基本类型: {key}")));
    }
    let (val, tail) = read_one(rest)?;
    if !tail.is_empty() {
        return Err(Error::Protocol(format!("字典签名多余内容: {tail}")));
    }
    Ok((key, val))
}

/// `as` -> `"s"`；`a{sv}` 不适用（请用 [`dict_types`]）
pub fn array_elem(t: &str) -> Result<&str> {
    if !t.starts_with('a') {
        return Err(Error::Protocol(format!("不是数组签名: {t}")));
    }
    let (elem, rest) = read_one(&t[1..])?;
    if !rest.is_empty() {
        return Err(Error::Protocol(format!("数组签名多余内容: {rest}")));
    }
    Ok(elem)
}

/// `(yv)` -> `["y", "v"]`
pub fn struct_fields(t: &str) -> Result<Vec<&str>> {
    if !t.starts_with('(') || !t.ends_with(')') {
        return Err(Error::Protocol(format!("不是结构体签名: {t}")));
    }
    split_all(&t[1..t.len() - 1])
}

fn is_basic_first(t: &str) -> bool {
    t.len() == 1 && is_single(t.as_bytes()[0]) && t != "v"
}

/// 递归扫描一个完整类型，结束时 `i` 指向该类型之后
fn scan(b: &[u8], i: &mut usize) -> Result<()> {
    if *i >= b.len() {
        return Err(Error::Protocol("签名意外结束".into()));
    }
    match b[*i] {
        b'a' => {
            *i += 1;
            if *i < b.len() && b[*i] == b'{' {
                // 字典：a{kv}
                *i += 1;
                scan(b, i)?; // key
                scan(b, i)?; // value
                if *i >= b.len() || b[*i] != b'}' {
                    return Err(Error::Protocol("字典签名缺少 '}'".into()));
                }
                *i += 1;
            } else {
                // 普通数组
                scan(b, i)?;
            }
        }
        b'(' => {
            *i += 1;
            loop {
                if *i >= b.len() {
                    return Err(Error::Protocol("结构体签名缺少 ')'".into()));
                }
                if b[*i] == b')' {
                    *i += 1;
                    break;
                }
                scan(b, i)?;
            }
        }
        c if is_single(c) => *i += 1,
        c => return Err(Error::Protocol(format!("非法签名字符 '{}'", c as char))),
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn read_one_basic() {
        assert_eq!(read_one("s").unwrap(), ("s", ""));
        assert_eq!(read_one("si").unwrap(), ("s", "i"));
        assert_eq!(read_one("ias").unwrap(), ("i", "as"));
    }

    #[test]
    fn read_one_nested() {
        assert_eq!(read_one("a{sv}").unwrap(), ("a{sv}", ""));
        assert_eq!(read_one("a{sv}s").unwrap(), ("a{sv}", "s"));
        assert_eq!(read_one("(yv)").unwrap(), ("(yv)", ""));
        assert_eq!(read_one("aa{si}u").unwrap(), ("aa{si}", "u"));
        assert_eq!(read_one("(a{sv}(ss))").unwrap(), ("(a{sv}(ss))", ""));
    }

    #[test]
    fn reject_broken_signature() {
        assert!(read_one("a{sv").is_err());
        assert!(read_one("(ss").is_err());
        assert!(read_one("z").is_err());
        assert!(read_one("").is_err());
    }

    #[test]
    fn helpers() {
        assert_eq!(dict_types("a{sv}").unwrap(), ("s", "v"));
        assert_eq!(dict_types("a{sa{sv}}").unwrap().1, "a{sv}");
        assert_eq!(array_elem("as").unwrap(), "s");
        assert_eq!(array_elem("a(yv)").unwrap(), "(yv)");
        assert_eq!(struct_fields("(yv)").unwrap(), vec!["y", "v"]);
        assert!(dict_types("as").is_err());
    }

    #[test]
    fn alignments() {
        assert_eq!(alignment_of("y"), 1);
        assert_eq!(alignment_of("s"), 4);
        assert_eq!(alignment_of("x"), 8);
        assert_eq!(alignment_of("a{sv}"), 4);
        assert_eq!(alignment_of("(yv)"), 8);
    }
}
