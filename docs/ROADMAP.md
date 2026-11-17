# ChimeraIRC roadmap

Where the project is and where it is going. These are plans and ideas, not promises, and the order can change.

**Current release: 0.1.0, an early release for Windows 10 and 11 (64-bit).**

## ✅ Working today

**Connecting and security**
- TLS by default, Strict Transport Security, client certificates
- SASL: PLAIN, SCRAM-SHA-256 and EXTERNAL
- Automatic reconnect with growing delays; passwords are never stored

**IRCv3**
- `server-time`, `message-tags`, `echo-message`, `extended-join`, `multi-prefix`, `userhost-in-names`
- `account-notify`, `away-notify`, `chghost`, `setname`, `batch` (netsplit and netjoin summaries)
- `chathistory` with `/history before|after|around|between`, typing indicators, `MONITOR`

**Windows and panes**
- MDI chat windows with a topic bar and a nick list that shows away marks
- Docked panes (Servers, Events, Messages, Notices, URLs, Channels, Connections, Users, Buddies, Output):
  drag, split, float, reorder, and save five layouts
- Light and dark themes (flat, switchable at run time, or follow Windows), custom chat colors and fonts, toolbar icons
  drawn for each theme

**Chatting**
- Tab completion, highlights, clickable links with a "where does this really go" hint, double-click a channel or nick
- Find in window, log search, multi-line paste protection, paste a picture or drop a file to send it over DCC
- Aliases, `/uptime`, `/say`, away and back, and services messages kept out of the way (logins are masked)

**People and setup**
- Buddy list with groups, ignore list, favorites with auto-join
- Identities per network (nick, alternates, user name, real name, SASL account), auto-perform scripts
- DCC files and chats with resume, passive mode, IPv6, turbo, TLS, and UPnP / NAT-PMP / PCP port mapping
- Import of HydraIRC settings; automatic pick-up of winIRC settings

**Packaging**
- Per-user MSI installer that upgrades in place, and a portable ZIP
- Update check: Help > Check for Updates, and an optional once-a-day check at start (off by default); it only tells you
  and opens the release page, it never installs anything

## 🔜 Next

| Item | Notes |
|---|---|
| **Code signing** | Windows SmartScreen warns about unsigned downloads. Likely through SignPath (free for open source) or Microsoft's cloud signing. |
| **More HydraIRC import** | Command profiles, color schemes and sounds. |
| **Notification sounds** | Per-event sounds and a choice of what plays. |
| **Better highlights** | Regular expressions, per-channel rules. |
| **High-DPI check** | The program is DPI-aware; larger scales need more testing. |

## 💡 Ideas

- A **Linux version**. The protocol and engine crates (`chimerairc-proto`, `chimerairc-core`) contain no Windows-specific
  code; a new front end would sit on top of them.
- 32-bit Windows and ARM64 builds
- More IRCv3: `labeled-response`, `invite-notify`, message replies and reactions
- A small scripting or plugin system beyond aliases
- A built-in `identd` server

## ❌ Deliberately not done

- **Fetching a page to preview a link.** It would tell that site who looked at it. A hover shows where a link really
  goes instead.
- **Uploading pasted pictures to a web service.** A picture is sent to one person over DCC instead.
- **Storing passwords.** Never; you are asked each time.

## 🧩 How the code is organised

| Crate | Role |
|---|---|
| `chimerairc-proto` | Message parsing and encoding, IRCv3 tags, CTCP. No I/O. |
| `chimerairc-core` | The sans-IO session engine, the TLS transport, DCC and port mapping. |
| `chimerairc-gui` | The native Win32 program. |

Suggestions and bug reports are welcome as [GitHub issues](https://github.com/theG33K/chimerairc/issues).
