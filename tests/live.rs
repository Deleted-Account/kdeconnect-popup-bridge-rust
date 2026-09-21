//! 真机联调测试：需要本机有正在运行的会话总线和已配对的 KDE Connect 设备。
//!
//! 默认不执行（避免在没有桌面的环境里失败），运行方式：
//! ```sh
//! cargo test -- --ignored --nocapture
//! ```

use kdeconnect_bridge::dbus::Conn;

/// 探测第一个在线设备
fn first_device() -> Option<String> {
    let out = std::process::Command::new("kdeconnect-cli")
        .args(["-a", "--id-only"])
        .output()
        .ok()?;
    String::from_utf8_lossy(&out.stdout)
        .lines()
        .map(|l| l.trim().to_string())
        .find(|l| !l.is_empty())
}

#[test]
#[ignore]
fn handshake_and_hello() {
    let mut conn = Conn::connect(None).expect("连接会话总线失败");
    let name = conn.hello().expect("Hello 失败");
    println!("unique name: {name}");
    // 总线分配的唯一名一定以 ':' 开头
    assert!(name.starts_with(':'), "唯一名异常: {name}");
}

#[test]
#[ignore]
fn add_match_and_list_notifications() {
    let Some(dev) = first_device() else {
        println!("跳过：没有在线设备");
        return;
    };
    let mut conn = Conn::connect(None).expect("连接失败");
    conn.hello().expect("Hello 失败");
    conn.add_match("type='signal',interface='org.kde.kdeconnect.device.notifications'")
        .expect("AddMatch 失败");

    let path = format!("/modules/kdeconnect/devices/{}/notifications", dev);
    let body = conn
        .method_call(
            "org.kde.kdeconnect",
            &path,
            "org.kde.kdeconnect.device.notifications",
            "activeNotifications",
            vec![],
        )
        .expect("activeNotifications 失败");
    println!("activeNotifications: {body:?}");
}

#[test]
#[ignore]
fn get_all_device_properties() {
    // 设备对象上的 a{sv} 属性是对字典解析最好的真实验证
    let Some(dev) = first_device() else {
        println!("跳过：没有在线设备");
        return;
    };
    let mut conn = Conn::connect(None).expect("连接失败");
    conn.hello().expect("Hello 失败");

    let props = conn
        .get_all(
            "org.kde.kdeconnect",
            &format!("/modules/kdeconnect/devices/{}", dev),
            "org.kde.kdeconnect.device",
        )
        .expect("GetAll 失败");
    println!("device props: {props:?}");
    assert!(!props.is_empty(), "设备属性为空，字典解析可能有问题");
}
