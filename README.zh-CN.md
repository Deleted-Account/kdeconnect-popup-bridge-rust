# kdeconnect-popup-bridge

[English](README.md) | **简体中文**

> 让手机上**每一条**通知，都在 KDE 桌面上真正弹出来。
> 纯 Rust / 纯标准库实现，**零第三方依赖**，release 二进制约 500 KB。

## 它解决什么问题

微信、东方财富这类 App 在 Android 侧会**复用同一个通知 id**：连续几条消息表现为「原地更新同一条通知」。
KDE Connect 遇到这种情况只会更新通知历史，**不再弹窗**（很多情况下连 D-Bus 信号都不发），消息就静悄悄漏掉了。

另一些 App（典型是行情/资讯类）把正文塞进 `ticker` 字段、`title` 只有 App 名，KDE Connect 弹出来只剩一个标题。

本程序直接连到 KDE Connect 的 D-Bus 接口，**订阅信号 + 轮询比对内容指纹**双管齐下，内容一变就补弹一次，并把标题与正文补全。

## 特性

- **双通道检测**：D-Bus 信号（实时）+ 轮询兜底（默认 1s），微信连续消息不发信号也能抓到
- **内容指纹**：`(ticker, title, text)` 任一变化即视为新消息
- **grace 抑制**（默认 2s）：很多 App 会「先发标题、几百毫秒后补正文」，grace 窗口内的更新视为同一条消息的补全，避免一条消息弹两次
- **两种接管模式**：默认只补弹漏掉的；`--all` 连首条也接管（见下文）
- **按 App 配置**：音效文件、弹窗停留时长、紧急度、是否启用
- **弹窗后端可选**：`notify-send`（默认，兼容性最好）或 `native`（用本项目自带的 D-Bus 直接发 Notify，不经过 shell、不启外部进程）
- **断线自动重连**：指数退避，最长 30s
- **零第三方 crate**：D-Bus 线协议编解码（含 `a{sv}`、`(yv)` 嵌套签名与 8 字节对齐）和极简 JSON 解析都是自己实现的

## 环境要求

- Linux + KDE Plasma，手机已与 KDE Connect 配对
- Rust 1.56+（edition 2021）—— 仅编译需要
- `notify-send`（libnotify）—— 仅默认 `cli` 后端需要
- `paplay`（PulseAudio / PipeWire）—— 需要音效时才用到

## 编译安装

```bash
git clone https://github.com/Deleted-Account/kdeconnect-popup-bridge-rust.git
cd kdeconnect-popup-bridge-rust
cargo build --release
cp target/release/kdeconnect-popup-bridge ~/.local/bin/    # 文件名随意
```

也可以直接用 [Releases](https://github.com/Deleted-Account/kdeconnect-popup-bridge-rust/releases) 里预编译好的二进制，跳过 Rust 工具链。

## 用法

```
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
```

`--flag value` 与 `--flag=value` 两种写法都支持。

## 两种工作模式

**默认模式**（不改任何系统配置）：首条通知仍交给 KDE Connect 弹，本程序只补音效（KDE Connect 的弹窗不发声），并在后续内容更新时补弹。适合只想「不漏消息」的场景。

**`--all` 模式**（推荐给东方财富这类把正文藏在 `ticker` 里的 App）：

1. 关闭 KDE Connect 自带的弹窗，编辑 `~/.config/kdeconnect.notifyrc`：

   ```ini
   [Event/notification]
   Action=
   ```

2. 用 `--all` 启动本程序，首条通知也改由本程序弹，标题取 `title`、正文取 `text` 并补上 `ticker` 里多出来的部分。

## 开机自启

`~/.config/autostart/kdeconnect-popup-bridge.desktop`：

```ini
[Desktop Entry]
Type=Application
Name=KDE Connect 通知补弹
Comment=让手机通知的每一条都在 KDE 桌面弹窗
Exec=/home/你的用户名/.local/bin/kdeconnect-popup-bridge --all
Terminal=false
X-KDE-autostart-after=panel
X-DBUS-StartupType=unique
```

> 注意：`--all` 模式依赖会话总线，务必在 KDE 会话内启动（自启动或登录后再跑），不要放进 systemd --user 的早期阶段。

### 手机还没连上？照样会等到它上线

自启动时本程序要和 `kdeconnectd`、也要和网络抢时间，经常比它们先跑起来：守护进程还没完成设备握手，
或者手机还没接进同一个 Wi-Fi，此刻探测设备必然拿到空列表。
因此**不会因为一时探测不到就退出** —— 它会每 2s 重试一次，直到出现「已配对且在线」的设备再挂上去。
哪怕你十分钟之后才把手机连上，它也是从那一刻起照常工作。

可用 `KC_BRIDGE_DETECT_WAIT` 调整：

| 取值 | 行为 |
|---|---|
| 不设置 / `forever` / `always` | 一直等到手机上线（默认） |
| `0` / `never` | 只探测一次，探测不到就退出 —— 适合希望快速显式失败的脚本 |
| `<秒数>` | 最多等这么久 |

如果用了 `--device` 显式指定设备，则完全不做探测，自然也不存在等待。

XDG autostart 生成的 unit 里写死了 `Restart=no`，一旦非零退出就没有人再把它拉起来，
表现是「通知补弹无声无息地失效了，必须手动 restart」。加个 drop-in 让它自愈：

```ini
# ~/.config/systemd/user/app-kdeconnect\x2dpopup\x2dbridge@autostart.service.d/restart.conf
[Service]
Restart=on-failure
RestartSec=20
```

## 音效配置

配置文件：`~/.config/kdeconnect-popup-bridge/sounds.json`（可用环境变量 `KC_BRIDGE_CONFIG` 覆盖）。

字符串写法（够用）：

```json
{
  "apps": {
    "WeChat": "/usr/share/sounds/freedesktop/stereo/message.ogg"
  },
  "default": ""
}
```

对象写法（可逐 App 细调，两种写法能混用）：

```json
{
  "apps": {
    "WeChat": { "sound": "/path/to/a.ogg", "timeout": 15, "urgency": "critical", "enabled": true }
  },
  "default": { "sound": "", "timeout": 10 }
}
```

- `sound` 为空字符串 = 静音；`default` 决定未匹配到的 App 的行为
- `timeout`：弹窗停留**秒数**，支持小数（如 `2.5`）；省略则回退到命令行 `--timeout`，再其次是内置默认 10 秒
- `urgency`：`low` / `normal` / `critical`
- `enabled: false` = 直接忽略该 App

App 名填的是 KDE Connect 上报的 `appName`（先精确匹配，再忽略大小写匹配），不确定就让它打出来看：
`kdeconnect-popup-bridge --dump`。最终取值是「`default` 打底 + App 规则逐项覆盖」，优先级为：

**App 专属规则 > `default` > 命令行 `--timeout` > 内置默认 10 秒**

三个容易踩的坑：

- 别写 `timeout: 0`。0 的语义在 freedesktop 规范里是乱的（`notify-send` 当立即关闭，别的实现当永不过期）。想用系统默认就删掉这个字段
- 配了 `urgency: "critical"` 就别指望 `timeout` 生效 —— KDE 不会让 critical 通知自动收起
- 这是**严格 JSON，不支持注释**：写 `//` 或 `/* */` 会让整个配置失效、程序启动即失败（症状是开机后完全没有补弹）

改完文件后**必须重启**才生效，配置只在进程启动时读一次：

```bash
systemctl --user restart 'app-kdeconnect\x2dpopup\x2dbridge@autostart.service'
```

也可以不手改文件，用命令行写：

```bash
kdeconnect-popup-bridge --set-sound WeChat=/path/to/a.ogg --set-sound default= --list-sounds
```

> `--list-sounds` 只显示音效，看不出 `timeout` / `urgency` —— 验证那两项请发一条真实通知。

更多写法变体（含混用示例、环境变量、App 名怎么查）见仓库里的 `sounds.json.example`。
那份是文档文件、带注释便于阅读，**不要直接 cp 成配置**。
英文用户请改用 `sounds.json.example.en`。

## 工作原理

```text
D-Bus 会话总线
  └─ org.kde.kdeconnect
       └─ device/<id> 的 org.kde.kdeconnect.device.notifications
            ├─ 信号：notificationPosted / Updated / Removed   ← 实时通道
            └─ 方法：activeNotifications                      ← 轮询兜底
                 └─ 逐条取 ticker / title / text → 算内容指纹
                      └─ 与上次比对 → decide() 决定 弹窗 / 只放音效 / 跳过
```

模块划分：

| 模块 | 职责 |
| --- | --- |
| `dbus::value` / `signature` / `marshal` / `unmarshal` / `message` / `Conn` | 自研 D-Bus 协议层（含 SASL EXTERNAL 认证） |
| `json` | 极简 JSON 解析/生成，够读写配置即可 |
| `config` | 音效规则（按 App，支持 timeout / urgency / enabled） |
| `notify` | 弹窗后端（`notify-send` / 原生 D-Bus Notify）与播放后端（`paplay`） |
| `bridge` | 业务状态机：内容指纹、去重、grace 抑制、弹窗与音效派发 |
| `cli` | 命令行解析 |

## 开发

```bash
cargo test                    # 单元测试 + 协议测试（46 个，不需要真机）
cargo test -- --ignored       # tests/live.rs：需真实 KDE Connect 会话
cargo run -- --dump           # 打印手机当前通知，验证协议连通性
cargo run -- --all --verbose  # 带调试信息运行
```

## 许可证

MIT，见 [LICENSE](LICENSE)。
