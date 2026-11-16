# ChimeraIRC

A Windows IRC client written in Rust, based on HydraIRC (GPLv3, unmaintained since ~2014).
Goal: keep HydraIRC's look and workflow (MDI windows, docked panes, classic menus/toolbar),
rebuilt on a modern, memory-safe, standards-current foundation.

> **Renamed.** This project was called winIRC until it became ChimeraIRC. On first start ChimeraIRC takes over
> the settings from `%APPDATA%\winIRC\settings.ini` (buddies, layouts, servers, ignores and so on) and saves them as
> `%APPDATA%\ChimeraIRC\settings.ini`; the old file is left where it is. Logs are now written under
> `%APPDATA%\ChimeraIRC\logs` unless a folder is chosen in Preferences, so earlier ones stay in the old folder.
> The environment variables for testing changed prefix too (`WINIRC_*` became `CHIMERAIRC_*`).

## Layout

| Path | Purpose |
|---|---|
| `crates/chimerairc-proto` | Wire protocol: message parse/encode, IRCv3 tags, CTCP. No I/O, no deps. |
| `crates/chimerairc-core` | Client engine: sans-IO `Session` (CAP 302, SASL PLAIN, ISUPPORT, channel/member/mode tracking, CTCP, ignore filtering), a tokio + rustls transport (`net::spawn`), the DCC transports (files, chat, resume, passive, IPv6, turbo, TLS) and router port forwarding (PCP, NAT-PMP, UPnP). |
| `crates/chimerairc-gui` | Native Win32 front end (`chimerairc.exe`): MDI frame, tab bar, toolbar, status bar, server/channel/query/DCC-chat windows with topic bar and user list, `/commands`, highlights, logging, channel list, favorites, ignore list, HydraIRC import, DCC transfers window. |
| `packaging/` | Builds the MSI installer and the portable ZIP (see `packaging/README.md`). |
| `docs/ROADMAP.md` | Feature map from HydraIRC to ChimeraIRC, and the standards plan. |

## Build

```
cargo test --workspace
cargo run -p chimerairc
```

In the app, use **File > Connect** (Ctrl+S), **Join Channel** (Ctrl+J) and **Options > Preferences** (Ctrl+P), or type `/help`. Connect with `/server irc.libera.chat` (TLS, port 6697), then `/join #channel`.
Use `/server -plain host 6667` for a non-TLS server.

`rust-toolchain.toml` pins the GNU toolchain because MSVC build tools aren't installed on the dev machine.

## License

GPL-3.0-or-later, inherited from HydraIRC (see `LICENSE`). Original copyright remains with the HydraIRC authors.
