# Scripting ChimeraIRC with Lua

ChimeraIRC runs small [Lua](https://www.lua.org/) scripts. A script can answer messages, add `/commands` of its own,
react to joins and connections, and run on a timer.

Scripts are **chat only**. They cannot read or write files, use the network, or start programs: only Lua's string,
table, math, utf8 and coroutine libraries are available. A script that runs too long is stopped, and one that sends
too fast is cut off, so a mistake cannot freeze the program or flood a server.

## Getting started

1. Choose **Options → Open Scripts Folder**. It is `%AppData%\ChimeraIRC\scripts`.
2. The first time, the folder holds `example.lua.off`. Rename it to `example.lua`.
3. Choose **Options → Reload Scripts** (or type `/script reload`).

Every `*.lua` file in the folder is loaded when ChimeraIRC starts and when you reload. Errors are shown in the window
with the script's name; a script with an error is left out and the others still work.

```lua
-- answer "!ping" in the channel it was said in
chimera.on("message", function(m)
  if not m.mine and not m.private and m.text:lower():find("!ping", 1, true) then
    chimera.say("pong, " .. m.nick)
  end
end)

-- a /hello command
chimera.command("hello", function(args)
  chimera.echo("Hello, " .. (args ~= "" and args or "world"))
end)
```

## Events: `chimera.on(event, function(e) ... end)`

| Event | Fields of `e` |
|---|---|
| `message` | `nick`, `target`, `text`, `private` (a private chat), `action` (a `/me`), `mine` (you said it), `network` |
| `notice` | the same as `message` |
| `join` | `nick`, `channel`, `mine` |
| `part` | `nick`, `channel`, `mine` |
| `connect` | `nick` |
| `disconnect` | none |
| `input` | the handler gets the typed line as text; **return `true`** to keep it from being sent or run |

`text` has the colors and bold codes removed. Messages you send are reported with `mine = true` (servers that echo them
back, and your own lines), so test `m.mine` before answering to avoid talking to yourself.

## What a script can do

| Function | What it does |
|---|---|
| `chimera.echo(text)` or `print(text)` | Show a line in the window, to you only. |
| `chimera.say(text)` | Say something in the window the event happened in (or the current one). |
| `chimera.me(text)` | Do an action (`/me`). |
| `chimera.msg(target, text)` | Send a private or channel message. |
| `chimera.notice(target, text)` | Send a notice. |
| `chimera.raw(line)` | Send a raw IRC line. |
| `chimera.run(line)` | Run any input line as if you typed it, for example `chimera.run("/join #rust")`. |
| `chimera.command(name, function(args) ... end)` | Add `/name`. `args` is the text after it. Built-in commands cannot be replaced. |
| `chimera.timer(seconds, fn, repeat)` | Call `fn` after `seconds` (at least 0.5); every time if `repeat` is `true`. Returns an id. |
| `chimera.cancel(id)` | Stop a timer. |
| `chimera.nick()`, `chimera.target()`, `chimera.network()` | Your nick, the channel or person of the window, and the server name. |
| `chimera.save(name, value)`, `chimera.load(name, default)` | Keep small text values between runs (`save(name, nil)` forgets one). |

Text a script sends is made into one clean line (line breaks become spaces) and cut to 1000 characters.

## Commands

| Command | What it does |
|---|---|
| `/script` | List the loaded scripts and the commands they added. |
| `/script reload` | Load all scripts again. |
| `/script unload <name>` | Stop one script. |
| `/script folder` | Open the scripts folder. |

## Limits

- A call may run about five million Lua instructions; after that it is stopped with "ran for too long".
- All scripts together may use 48 MB of memory, 50 timers and 200 saved values each.
- A script may send 30 lines per second.
- Lines a script runs are not offered to `input` handlers again, so scripts cannot loop forever through each other.
- Saved values live in `%AppData%\ChimeraIRC\scripts\data\<script>.tsv`.

Only run scripts you trust. They cannot reach your files, but a script can say things in your name.
