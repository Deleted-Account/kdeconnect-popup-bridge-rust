# kdeconnect-popup-bridge

**English** | [简体中文](README.zh-CN.md)

> Make **every** notification from your phone actually pop up on the KDE desktop.
> Pure Rust / pure `std`, **zero third-party crates**. Release binary ≈ 500 KB.

## The problem

Apps like WeChat and Eastmoney **reuse the same notification id** on Android: several
messages in a row arrive as "the same notification being updated in place". When that
happens KDE Connect only updates the notification history and **does not pop anything up**
(often it does not even emit a D-Bus signal), so messages silently slip by.

Other apps (typically market/news apps) put the real content in the `ticker` field and
leave only the app name in `title`, so KDE Connect pops up a title with no body.

This program talks to the KDE Connect D-Bus interface directly and runs **two channels at
once — signal subscription plus polling with content fingerprints**. Whenever the content
changes it pops a notification, with the title and body properly filled in.

## Features

- **Two channels**: D-Bus signals (real-time) + polling fallback (default 1s), so
  consecutive WeChat messages that emit no signal are still caught
- **Content fingerprint**: `(ticker, title, text)` — a change in any field counts as a new message
- **Grace suppression** (default 2s): many apps send the title first and append the body
  a few hundred milliseconds later. Updates inside the grace window are treated as
  completion of the same message, so one message never becomes two popups
- **Two takeover modes**: by default it only re-pops what KDE Connect missed; `--all`
  takes over even the first notification (see below)
- **Per-app rules**: sound file, popup duration, urgency, enable/disable
- **Two popup backends**: `notify-send` (default, best compatibility) or `native`
  (sends `Notify` straight over this project's own D-Bus layer — no shell, no external process)
- **Auto reconnect** with exponential backoff (up to 30s)
- **Zero dependencies**: the D-Bus wire protocol (including `a{sv}` / `(yv)` nested
  signatures and 8-byte alignment) and the minimal JSON parser are both implemented here

## Requirements

- Linux + KDE Plasma, with KDE Connect already paired to your phone
- Rust 1.56+ (edition 2021) — build time only
- `notify-send` (libnotify) — only for the default `cli` backend
- `paplay` (PulseAudio / PipeWire) — only if you want sounds

## Build & install

```bash
git clone https://github.com/Deleted-Account/kdeconnect-popup-bridge-rust.git
cd kdeconnect-popup-bridge-rust
cargo build --release
cp target/release/kdeconnect-popup-bridge ~/.local/bin/    # name it whatever you like
```

You can also grab the prebuilt binary from
[Releases](https://github.com/Deleted-Account/kdeconnect-popup-bridge-rust/releases)
and skip the Rust toolchain entirely.

## Usage

```
Usage:
  kdeconnect-popup-bridge [options]

Run:
      --all                 Let this program pop the first notification too
                            (requires turning off KDE Connect's own popup first)
      --app <name>          Only handle the given app; repeatable, e.g. --app WeChat
      --device <id>         Only handle the given device; repeatable
                            (default: auto-detect every reachable device)
      --interval <sec>      Polling interval, default 1.0
      --grace <sec>         Do not re-pop within N seconds of the first popup,
                            default 2.0 (stops the first message from popping twice)
      --timeout <sec>       How long the popup stays, default 10
      --notifier <backend>  cli (default, calls notify-send) or native (raw D-Bus Notify)
      --bus <address>       Override DBUS_SESSION_BUS_ADDRESS, e.g. /run/user/1000/bus
      --verbose             Print debug info (including raw D-Bus properties)
  -h, --help                Show this help

Sound:
      --no-sound            Mute everything
      --sound <file>        One-off sound (lower priority than per-app rules)
      --set-sound <App=path>  Write a sound rule; repeatable.
                            default= means "mute every other app";
                            App= deletes that rule. e.g. --set-sound WeChat=/a/b.ogg
      --list-sounds         Print the current rules and exit

Debug:
      --dump                Print the notifications currently on the phone and exit
```

Both `--flag value` and `--flag=value` are accepted.

## Two working modes

**Default mode** (no system changes needed): the first notification is still popped by
KDE Connect; this program only adds the sound (KDE Connect's popup is silent) and re-pops
later content updates. Good if you just want "no missed messages".

**`--all` mode** (recommended for apps like Eastmoney that hide the body in `ticker`):

1. Turn off KDE Connect's own popup by editing `~/.config/kdeconnect.notifyrc`:

   ```ini
   [Event/notification]
   Action=
   ```

2. Start this program with `--all`. The first notification is popped by us as well:
   the title comes from `title`, the body from `text` plus whatever extra the `ticker` carries.

## Autostart

`~/.config/autostart/kdeconnect-popup-bridge.desktop`:

```ini
[Desktop Entry]
Type=Application
Name=KDE Connect notification bridge
Comment=Make every phone notification pop up on the KDE desktop
Exec=/home/your-user/.local/bin/kdeconnect-popup-bridge --all
Terminal=false
X-KDE-autostart-after=panel
X-DBUS-StartupType=unique
```

> `--all` depends on the session bus, so start it inside the KDE session (autostart, or
> after login). Don't put it in an early systemd `--user` unit.

## Sound rules

Config file: `~/.config/kdeconnect-popup-bridge/sounds.json`
(override the path with the `KC_BRIDGE_CONFIG` environment variable).

String form (all most people need):

```json
{
  "apps": {
    "WeChat": "/usr/share/sounds/freedesktop/stereo/message.ogg"
  },
  "default": ""
}
```

Object form (per-app fine-tuning; both forms can be mixed in one file):

```json
{
  "apps": {
    "WeChat": { "sound": "/path/to/a.ogg", "timeout": 15, "urgency": "critical", "enabled": true }
  },
  "default": { "sound": "", "timeout": 10 }
}
```

- An empty `sound` string means mute; `default` governs every app without its own rule
- `timeout`: how long the popup stays, in **seconds**; decimals are fine (`2.5`).
  When omitted it falls back to `--timeout`, then to the built-in default of 10 seconds
- `urgency`: `low` / `normal` / `critical`
- `enabled: false` ignores that app entirely

The app key is the `appName` KDE Connect reports (matched exactly first, then
case-insensitively). Unsure what it is? Print it: `kdeconnect-popup-bridge --dump`.
The effective rule is `default` plus whatever the app rule overrides, so precedence is:

**app rule > `default` > command line `--timeout` > built-in default of 10 seconds**

Three things worth knowing:

- Don't write `timeout: 0`. The freedesktop spec is inconsistent about it (`notify-send`
  treats 0 as "expire immediately", other implementations as "never"). Omit the field to
  get the system default
- Combining `urgency: "critical"` with `timeout` is pointless — KDE keeps critical
  notifications up until dismissed
- This is **strict JSON with no comments**. A stray `//` makes the whole config invalid
  and the program exits at startup (symptom: no bridging at all after login)

The config is read once at startup, so **restart** after editing it:

```bash
systemctl --user restart 'app-kdeconnect\x2dpopup\x2dbridge@autostart.service'
```

You can skip editing the file and write rules from the CLI:

```bash
kdeconnect-popup-bridge --set-sound WeChat=/path/to/a.ogg --set-sound default= --list-sounds
```

> `--list-sounds` only prints sounds, not `timeout` / `urgency` — send yourself a real
> notification to verify those two.

For every other form (mixing string and object entries, `KC_BRIDGE_CONFIG`, how to find an
app name), see `sounds.json.example.en` in this repo. That file is documentation
— it carries comments, so don't copy it over your config.

## How it works

```text
D-Bus session bus
  └─ org.kde.kdeconnect
       └─ org.kde.kdeconnect.device.notifications on device/<id>
            ├─ signals: notificationPosted / Updated / Removed   ← real-time channel
            └─ method:  activeNotifications                      ← polling fallback
                 └─ read ticker / title / text → content fingerprint
                      └─ compare with last time → decide():
                         popup / sound only / skip
```

Modules:

| Module | Role |
| --- | --- |
| `dbus::value` / `signature` / `marshal` / `unmarshal` / `message` / `Conn` | Hand-written D-Bus protocol layer (incl. SASL EXTERNAL auth) |
| `json` | Minimal JSON parse/serialize, just enough for the config |
| `config` | Sound rules per app (sound / timeout / urgency / enabled) |
| `notify` | Popup backends (`notify-send`, native D-Bus Notify) and player backend (`paplay`) |
| `bridge` | State machine: fingerprints, dedup, grace suppression, popup & sound dispatch |
| `cli` | Command line parsing |

## Development

```bash
cargo test                    # unit + protocol tests (46, no phone needed)
cargo test -- --ignored       # tests/live.rs: needs a real KDE Connect session
cargo run -- --dump           # print current notifications, verifies the protocol path
cargo run -- --all --verbose  # run with debug output
```

## License

MIT — see [LICENSE](LICENSE).
