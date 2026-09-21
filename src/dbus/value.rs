//! D-Bus 类型系统的最小实现（只依赖 std）。
//!
//! 设计要点：
//! - 数组/字典**必须携带元素签名**（`Array(元素签名, 元素)`），否则空数组无法推断类型，
//!   序列化时就写不出正确的签名。这是很多简化实现的坑。
//! - `Variant` 用 `Box` 打破递归类型的无限大小。

/// D-Bus 支持的所有类型（够用且完整）
#[derive(Debug, Clone, PartialEq)]
pub enum Value {
    /// `y` 无符号 8 位
    Byte(u8),
    /// `b` 布尔（线上是 UINT32，非 0 为真）
    Bool(bool),
    /// `n` INT16
    Int16(i16),
    /// `q` UINT16
    Uint16(u16),
    /// `i` INT32
    Int32(i32),
    /// `u` UINT32
    Uint32(u32),
    /// `x` INT64
    Int64(i64),
    /// `t` UINT64
    Uint64(u64),
    /// `d` DOUBLE
    Double(f64),
    /// `s` 字符串
    String(String),
    /// `o` 对象路径（线上格式等同字符串）
    ObjectPath(String),
    /// `g` 签名（线上是 1 字节长度 + NUL，不是 UINT32）
    Signature(String),
    /// `a…` 数组：`Array(元素签名, 元素列表)`
    Array(String, Vec<Value>),
    /// `(…)` 结构体
    Struct(Vec<Value>),
    /// `a{…}` 字典：`Dict(键签名, 值签名, 条目列表)`
    Dict(String, String, Vec<(Value, Value)>),
    /// `v` 变体
    Variant(Box<Value>),
}

impl Value {
    /// 计算该值的完整签名，序列化时写入 VARIANT 与消息头的 signature 字段
    pub fn signature(&self) -> String {
        match self {
            Value::Byte(_) => "y".to_string(),
            Value::Bool(_) => "b".to_string(),
            Value::Int16(_) => "n".to_string(),
            Value::Uint16(_) => "q".to_string(),
            Value::Int32(_) => "i".to_string(),
            Value::Uint32(_) => "u".to_string(),
            Value::Int64(_) => "x".to_string(),
            Value::Uint64(_) => "t".to_string(),
            Value::Double(_) => "d".to_string(),
            Value::String(_) => "s".to_string(),
            Value::ObjectPath(_) => "o".to_string(),
            Value::Signature(_) => "g".to_string(),
            Value::Array(elem, _) => format!("a{}", elem),
            Value::Struct(items) => {
                let inner: String = items.iter().map(|v| v.signature()).collect();
                format!("({})", inner)
            }
            Value::Dict(k, v, _) => format!("a{{{}{}}}", k, v),
            Value::Variant(_) => "v".to_string(),
        }
    }

    /// 取字符串（String / ObjectPath / Signature 都算）
    pub fn as_str(&self) -> Option<&str> {
        match self {
            Value::String(s) | Value::ObjectPath(s) | Value::Signature(s) => Some(s),
            _ => None,
        }
    }

    pub fn as_bool(&self) -> Option<bool> {
        match self {
            Value::Bool(b) => Some(*b),
            _ => None,
        }
    }

    pub fn as_u32(&self) -> Option<u32> {
        match self {
            Value::Uint32(v) => Some(*v),
            _ => None,
        }
    }

    pub fn as_i64(&self) -> Option<i64> {
        match self {
            Value::Int64(v) => Some(*v),
            Value::Int32(v) => Some(*v as i64),
            _ => None,
        }
    }

    pub fn as_byte(&self) -> Option<u8> {
        match self {
            Value::Byte(v) => Some(*v),
            _ => None,
        }
    }

    pub fn as_array(&self) -> Option<&Vec<Value>> {
        match self {
            Value::Array(_, items) => Some(items),
            _ => None,
        }
    }

    pub fn as_dict(&self) -> Option<&Vec<(Value, Value)>> {
        match self {
            Value::Dict(_, _, entries) => Some(entries),
            _ => None,
        }
    }

    /// 剥掉一层 Variant；不是 Variant 就返回自身
    pub fn unwrap_variant(&self) -> &Value {
        match self {
            Value::Variant(inner) => inner.unwrap_variant(),
            other => other,
        }
    }

    /// 便于日志打印的紧凑表示
    pub fn display(&self) -> String {
        match self {
            Value::String(s) | Value::ObjectPath(s) | Value::Signature(s) => s.clone(),
            Value::Bool(b) => b.to_string(),
            Value::Byte(v) => v.to_string(),
            Value::Int16(v) => v.to_string(),
            Value::Uint16(v) => v.to_string(),
            Value::Int32(v) => v.to_string(),
            Value::Uint32(v) => v.to_string(),
            Value::Int64(v) => v.to_string(),
            Value::Uint64(v) => v.to_string(),
            Value::Double(v) => v.to_string(),
            Value::Array(elem, items) => format!(
                "a{}[{}]",
                elem,
                items.iter().map(|v| v.display()).collect::<Vec<_>>().join(", ")
            ),
            Value::Struct(items) => format!(
                "({})",
                items.iter().map(|v| v.display()).collect::<Vec<_>>().join(", ")
            ),
            Value::Dict(_, _, entries) => format!(
                "{{{}}}",
                entries
                    .iter()
                    .map(|(k, v)| format!("{}: {}", k.display(), v.display()))
                    .collect::<Vec<_>>()
                    .join(", ")
            ),
            Value::Variant(inner) => format!("v({})", inner.display()),
        }
    }
}

/// 便捷构造：字符串
pub fn vstr(s: impl Into<String>) -> Value {
    Value::String(s.into())
}

/// 便捷构造：变体
pub fn vvar(v: Value) -> Value {
    Value::Variant(Box::new(v))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn signature_of_scalars() {
        assert_eq!(Value::Byte(1).signature(), "y");
        assert_eq!(Value::Bool(true).signature(), "b");
        assert_eq!(Value::Int32(-5).signature(), "i");
        assert_eq!(Value::Uint64(7).signature(), "t");
        assert_eq!(vstr("hi").signature(), "s");
    }

    #[test]
    fn signature_of_containers() {
        // 空数组也能算出签名，这正是数组要带元素签名的原因
        assert_eq!(Value::Array("s".into(), vec![]).signature(), "as");
        assert_eq!(
            Value::Dict("s".into(), "v".into(), vec![]).signature(),
            "a{sv}"
        );
        assert_eq!(
            Value::Struct(vec![Value::Byte(1), vvar(vstr("x"))]).signature(),
            "(yv)"
        );
        assert_eq!(Value::Array("(yv)".into(), vec![]).signature(), "a(yv)");
    }

    #[test]
    fn unwrap_variant_is_recursive() {
        let v = vvar(vvar(vstr("deep")));
        assert_eq!(v.unwrap_variant().as_str(), Some("deep"));
    }
}
