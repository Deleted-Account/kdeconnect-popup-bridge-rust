//! 协议层集成测试：序列化 -> 反序列化 必须完全还原（含大端/小端两种字节序）。
//!
//! 这些用例从「库的外部使用者」视角验证，是手写 D-Bus 实现的主要防线。

use kdeconnect_bridge::dbus::marshal::Writer;
use kdeconnect_bridge::dbus::unmarshal::Reader;
use kdeconnect_bridge::dbus::value::{vstr, vvar, Value};

/// 序列化后按签名解析回来，断言与原始值相等
fn roundtrip(v: &Value, sig: &str, le: bool) {
    let mut w = Writer::with_endian(le);
    w.value(v);
    let bytes = w.into_inner();
    let mut r = Reader::new(&bytes, le);
    let got = r.value(sig).expect("解析失败");
    assert_eq!(&got, v, "签名 {sig} 在 {} 端序下往返不一致", if le { "小" } else { "大" });
    // 同时校验长度计算：解析后应正好读完
    assert_eq!(r.remaining(), 0, "签名 {sig} 解析后有残留字节");
}

fn both_endians(v: &Value, sig: &str) {
    roundtrip(v, sig, true);
    roundtrip(v, sig, false);
}

#[test]
fn scalars() {
    both_endians(&Value::Byte(200), "y");
    both_endians(&Value::Bool(true), "b");
    both_endians(&Value::Bool(false), "b");
    both_endians(&Value::Int16(-300), "n");
    both_endians(&Value::Uint16(60000), "q");
    both_endians(&Value::Int32(-70000), "i");
    both_endians(&Value::Uint32(4_000_000_000), "u");
    both_endians(&Value::Int64(-9_000_000_000), "x");
    both_endians(&Value::Uint64(18_000_000_000), "t");
    both_endians(&Value::Double(std::f64::consts::PI), "d");
    both_endians(&vstr("hello 世界"), "s");
    both_endians(&Value::ObjectPath("/org/freedesktop/DBus".into()), "o");
    both_endians(&Value::Signature("a{sv}".into()), "g");
}

#[test]
fn strings_with_utf8_and_empty() {
    both_endians(&vstr(""), "s");
    both_endians(&vstr("中文 emoji 🎧 混排"), "s");
    both_endians(&vstr("a\nb\tc"), "s");
}

#[test]
fn arrays() {
    both_endians(&Value::Array("s".into(), vec![vstr("a"), vstr("bb"), vstr("ccc")]), "as");
    // 空数组：依赖元素签名才能正确还原
    both_endians(&Value::Array("s".into(), vec![]), "as");
    both_endians(&Value::Array("y".into(), vec![Value::Byte(1), Value::Byte(2)]), "ay");
    // 结构体数组
    both_endians(
        &Value::Array(
            "(yv)".into(),
            vec![
                Value::Struct(vec![Value::Byte(1), vvar(vstr("/path"))]),
                Value::Struct(vec![Value::Byte(6), vvar(vstr("dest"))]),
            ],
        ),
        "a(yv)",
    );
}

#[test]
fn dicts() {
    let dict = Value::Dict(
        "s".into(),
        "v".into(),
        vec![
            (vstr("appName"), vvar(vstr("WeChat"))),
            (vstr("hasIcon"), vvar(Value::Bool(true))),
            (vstr("id"), vvar(Value::Int64(-12345))),
            (vstr("tags"), vvar(Value::Array("s".into(), vec![vstr("x")]))),
        ],
    );
    both_endians(&dict, "a{sv}");
    // 空字典（Notify 的 hints 就是空的 a{sv}）
    both_endians(&Value::Dict("s".into(), "v".into(), vec![]), "a{sv}");
}

#[test]
fn nested_variants() {
    both_endians(&vvar(vvar(vstr("deep"))), "v");
    both_endians(&Value::Struct(vec![Value::Byte(3), vvar(Value::Uint32(9))]), "(yv)");
}

#[test]
fn struct_with_mixed_fields() {
    let s = Value::Struct(vec![vstr("abc"), Value::Uint32(7), Value::Bool(false)]);
    both_endians(&s, "(sub)");
}

#[test]
fn malformed_input_errors_instead_of_panicking() {
    // 声明长度远大于实际数据
    let mut w = Writer::new();
    w.string("short");
    let bytes = w.into_inner();
    let mut r = Reader::new(&bytes[..2], true);
    assert!(r.value("s").is_err());

    // 非法签名
    let mut r2 = Reader::new(&[0u8; 8], true);
    assert!(r2.value("z").is_err());
}

#[test]
fn alignment_is_respected() {
    // 先写 1 字节，再写 UINT32：中间应有 3 字节填充
    let mut w = Writer::new();
    w.u8(1);
    w.u32(0x11223344);
    let bytes = w.into_inner();
    assert_eq!(bytes.len(), 8);
    assert_eq!(&bytes[1..4], &[0, 0, 0]);

    // STRING 前有 4 字节对齐，长度字段在小端下应为 03 00 00 00
    let mut w2 = Writer::new();
    w2.u8(0);
    w2.string("abc");
    let b2 = w2.into_inner();
    assert_eq!(&b2[4..8], &[3, 0, 0, 0]);
}
