//! Lua scripting. Scripts are `.lua` files in `%AppData%\ChimeraIRC\scripts`; they react to chat events, add
//! `/commands`, set timers, and answer by printing or by running input lines. They cannot touch the PC: no files,
//! no network, no other programs (only the string, table, math, utf8 and coroutine libraries are there), and every
//! call has an instruction budget so a loop that never ends is stopped.
//!
//! This module is pure (no windows): a script's answers come back as [`Action`]s for the program to carry out, and
//! the program tells the scripts what happened through `fire`, `command`, `input` and `tick`.
//!
//! ```lua
//! chimera.on("message", function(m)
//!   if m.text:find("!ping", 1, true) and not m.mine then chimera.say("pong, " .. m.nick) end
//! end)
//! chimera.command("hello", function(args) chimera.echo("hello " .. args) end)
//! chimera.timer(60, function() chimera.echo("a minute passed") end, true)
//! ```

use mlua::{Function, HookTriggers, Lua, LuaOptions, RegistryKey, StdLib, Table, Value, VmState};
use std::cell::{Cell, RefCell};
use std::collections::{BTreeMap, HashMap};
use std::path::{Path, PathBuf};
use std::rc::Rc;

/// What a script asked for.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Action {
    /// Print a line in the window.
    Echo(String),
    /// Run an input line in the window, as if the user had typed it (`/msg bob hi`, `/say hello`, `/raw ...`).
    Run(String),
}

/// Where an event happened, so `chimera.nick()`, `chimera.target()` and `chimera.network()` can answer.
#[derive(Clone, Debug, Default)]
pub struct Ctx {
    pub nick: String,
    pub target: String,
    pub network: String,
}

/// A piece of event data for a script (shown as a field of the event table).
#[derive(Clone, Debug)]
pub enum Val {
    S(String),
    B(bool),
}

impl From<&str> for Val {
    fn from(s: &str) -> Val {
        Val::S(s.to_owned())
    }
}
impl From<String> for Val {
    fn from(s: String) -> Val {
        Val::S(s)
    }
}
impl From<bool> for Val {
    fn from(b: bool) -> Val {
        Val::B(b)
    }
}

/// Instructions one call may run before it is stopped.
const BUDGET: u64 = 5_000_000;
/// Instructions between two looks at the budget.
const STEP: u32 = 10_000;
/// Memory all scripts together may use.
const MEMORY: usize = 48 << 20;
/// Actions allowed per second (a script that talks faster is cut off and told so).
const RATE: u32 = 30;
/// Longest text a script may send in one action.
const MAX_TEXT: usize = 1000;
const MAX_KEYS: usize = 200;

struct Handler {
    script: String,
    event: String,
    key: RegistryKey,
}

struct Command {
    script: String,
    name: String,
    key: RegistryKey,
}

struct Timer {
    id: u32,
    script: String,
    due: u64,
    every: Option<u64>,
    key: RegistryKey,
}

/// State shared with the functions scripts call.
#[derive(Default)]
struct Shared {
    out: RefCell<Vec<Action>>,
    ctx: RefCell<Ctx>,
    handlers: RefCell<Vec<Handler>>,
    commands: RefCell<Vec<Command>>,
    timers: RefCell<Vec<Timer>>,
    next_timer: Cell<u32>,
    /// The script whose code is running (registrations belong to it).
    current: RefCell<String>,
    now: Cell<u64>,
    budget: Cell<u64>,
    /// Start of the current second and the actions made in it.
    second: Cell<(u64, u32)>,
    flooded: Cell<bool>,
    data: RefCell<HashMap<String, BTreeMap<String, String>>>,
    data_dir: RefCell<Option<PathBuf>>,
}

impl Shared {
    fn push(&self, a: Action) {
        let (start, n) = self.second.get();
        let now = self.now.get();
        let (start, n) = if now.saturating_sub(start) >= 1000 { (now, 0) } else { (start, n) };
        if n >= RATE {
            if !self.flooded.replace(true) {
                self.out.borrow_mut().push(Action::Echo(format!("* [script {}] is sending too fast; the extra lines are dropped.", self.current.borrow())));
            }
            self.second.set((start, n));
            return;
        }
        if start != self.second.get().0 {
            self.flooded.set(false);
        }
        self.second.set((start, n + 1));
        self.out.borrow_mut().push(a);
    }
}

/// One line of text from a script, safe to put in an IRC message: no line breaks, not too long.
fn clean(s: &str) -> String {
    let one: String = s.chars().map(|c| if c == '\r' || c == '\n' || c == '\0' { ' ' } else { c }).collect();
    one.chars().take(MAX_TEXT).collect()
}

fn data_path(dir: &Path, script: &str) -> PathBuf {
    dir.join("data").join(format!("{script}.tsv"))
}

fn load_data(dir: &Path, script: &str) -> BTreeMap<String, String> {
    let mut m = BTreeMap::new();
    if let Ok(text) = std::fs::read_to_string(data_path(dir, script)) {
        for line in text.lines().take(MAX_KEYS) {
            if let Some((k, v)) = line.split_once('\t') {
                m.insert(k.to_owned(), v.replace("\\n", "\n").replace("\\\\", "\\"));
            }
        }
    }
    m
}

fn save_data(dir: &Path, script: &str, m: &BTreeMap<String, String>) {
    let path = data_path(dir, script);
    if let Some(parent) = path.parent() {
        let _ = std::fs::create_dir_all(parent);
    }
    let text: String = m.iter().map(|(k, v)| format!("{k}\t{}\n", v.replace('\\', "\\\\").replace('\n', "\\n"))).collect();
    let _ = std::fs::write(path, text);
}

pub struct Host {
    lua: Lua,
    sh: Rc<Shared>,
    scripts: Vec<String>,
}

fn add_functions(lua: &Lua, sh: &Rc<Shared>) -> mlua::Result<()> {
    let t = lua.create_table()?;
    t.set("version", env!("CARGO_PKG_VERSION"))?;

    let s = sh.clone();
    t.set(
        "on",
        lua.create_function(move |lua, (event, f): (String, Function)| {
            let key = lua.create_registry_value(f)?;
            s.handlers.borrow_mut().push(Handler { script: s.current.borrow().clone(), event: event.to_ascii_lowercase(), key });
            Ok(())
        })?,
    )?;
    let s = sh.clone();
    t.set(
        "command",
        lua.create_function(move |lua, (name, f): (String, Function)| {
            let name = name.trim_start_matches('/').to_ascii_lowercase();
            if name.is_empty() || name.contains(char::is_whitespace) {
                return Err(mlua::Error::runtime("a command name is one word, like \"hello\""));
            }
            let key = lua.create_registry_value(f)?;
            let mut cmds = s.commands.borrow_mut();
            cmds.retain(|c| c.name != name);
            cmds.push(Command { script: s.current.borrow().clone(), name, key });
            Ok(())
        })?,
    )?;
    let s = sh.clone();
    t.set("echo", lua.create_function(move |_, text: String| {
        s.push(Action::Echo(clean(&text)));
        Ok(())
    })?)?;
    let s = sh.clone();
    t.set("run", lua.create_function(move |_, line: String| {
        s.push(Action::Run(clean(&line)));
        Ok(())
    })?)?;
    // say(text) in this window, or msg(target, text)
    let s = sh.clone();
    t.set("say", lua.create_function(move |_, text: String| {
        s.push(Action::Run(format!("/say {}", clean(&text))));
        Ok(())
    })?)?;
    let s = sh.clone();
    t.set("me", lua.create_function(move |_, text: String| {
        s.push(Action::Run(format!("/me {}", clean(&text))));
        Ok(())
    })?)?;
    let s = sh.clone();
    t.set("msg", lua.create_function(move |_, (target, text): (String, String)| {
        s.push(Action::Run(format!("/msg {} {}", clean(&target).replace(' ', ""), clean(&text))));
        Ok(())
    })?)?;
    let s = sh.clone();
    t.set("notice", lua.create_function(move |_, (target, text): (String, String)| {
        s.push(Action::Run(format!("/notice {} {}", clean(&target).replace(' ', ""), clean(&text))));
        Ok(())
    })?)?;
    let s = sh.clone();
    t.set("raw", lua.create_function(move |_, line: String| {
        s.push(Action::Run(format!("/raw {}", clean(&line))));
        Ok(())
    })?)?;
    let s = sh.clone();
    t.set("nick", lua.create_function(move |_, ()| Ok(s.ctx.borrow().nick.clone()))?)?;
    let s = sh.clone();
    t.set("target", lua.create_function(move |_, ()| Ok(s.ctx.borrow().target.clone()))?)?;
    let s = sh.clone();
    t.set("network", lua.create_function(move |_, ()| Ok(s.ctx.borrow().network.clone()))?)?;

    // timer(seconds, fn [, repeat]) -> id; cancel(id)
    let s = sh.clone();
    t.set(
        "timer",
        lua.create_function(move |lua, (secs, f, repeat): (f64, Function, Option<bool>)| {
            if !(secs.is_finite() && secs >= 0.5) {
                return Err(mlua::Error::runtime("a timer needs at least half a second"));
            }
            if s.timers.borrow().len() >= 50 {
                return Err(mlua::Error::runtime("too many timers (50 at most)"));
            }
            let key = lua.create_registry_value(f)?;
            let ms = (secs * 1000.0) as u64;
            let id = s.next_timer.get() + 1;
            s.next_timer.set(id);
            s.timers.borrow_mut().push(Timer {
                id,
                script: s.current.borrow().clone(),
                due: s.now.get() + ms,
                every: repeat.unwrap_or(false).then_some(ms),
                key,
            });
            Ok(id)
        })?,
    )?;
    let s = sh.clone();
    t.set("cancel", lua.create_function(move |_, id: u32| {
        s.timers.borrow_mut().retain(|t| t.id != id);
        Ok(())
    })?)?;

    // save(key, value) / load(key [, default]): a few small values kept for this script between runs
    let s = sh.clone();
    t.set(
        "save",
        lua.create_function(move |_, (key, value): (String, Option<String>)| {
            let script = s.current.borrow().clone();
            if key.is_empty() || key.len() > 100 || key.contains(['\t', '\n', '\r']) {
                return Err(mlua::Error::runtime("a saved name is short text without tabs or line breaks"));
            }
            let mut all = s.data.borrow_mut();
            let m = all.entry(script.clone()).or_default();
            match value {
                Some(v) => {
                    if m.len() >= MAX_KEYS && !m.contains_key(&key) {
                        return Err(mlua::Error::runtime("too many saved values (200 at most)"));
                    }
                    m.insert(key, v.chars().take(2000).collect());
                }
                None => {
                    m.remove(&key);
                }
            }
            if let Some(dir) = s.data_dir.borrow().as_ref() {
                save_data(dir, &script, m);
            }
            Ok(())
        })?,
    )?;
    let s = sh.clone();
    t.set(
        "load",
        lua.create_function(move |_, (key, default): (String, Option<String>)| {
            let script = s.current.borrow().clone();
            let mut all = s.data.borrow_mut();
            if !all.contains_key(&script) {
                let m = s.data_dir.borrow().as_ref().map(|d| load_data(d, &script)).unwrap_or_default();
                all.insert(script.clone(), m);
            }
            Ok(all[&script].get(&key).cloned().or(default))
        })?,
    )?;

    let g = lua.globals();
    g.set("chimera", t)?;
    // print goes to the window
    let s = sh.clone();
    g.set(
        "print",
        lua.create_function(move |_, args: mlua::Variadic<Value>| {
            let parts: Vec<String> = args.iter().map(|v| v.to_string().unwrap_or_default()).collect();
            s.push(Action::Echo(clean(&parts.join(" "))));
            Ok(())
        })?,
    )?;
    // nothing that reads files, loads code from text or bytes, or reaches the system
    for name in ["dofile", "loadfile", "load", "loadstring", "require", "collectgarbage", "io", "os", "package", "debug"] {
        g.set(name, Value::Nil)?;
    }
    Ok(())
}

impl Host {
    /// A new, empty Lua state. `data_dir` is where scripts keep their saved values (none: kept in memory only).
    pub fn new(data_dir: Option<&Path>) -> Result<Host, String> {
        let libs = StdLib::STRING | StdLib::TABLE | StdLib::MATH | StdLib::UTF8 | StdLib::COROUTINE;
        let lua = Lua::new_with(libs, LuaOptions::default()).map_err(|e| e.to_string())?;
        let _ = lua.set_memory_limit(MEMORY);
        let sh = Rc::new(Shared::default());
        *sh.data_dir.borrow_mut() = data_dir.map(Path::to_path_buf);
        add_functions(&lua, &sh).map_err(|e| e.to_string())?;
        // every call has an instruction budget (set again before each call)
        let s = sh.clone();
        lua.set_hook(HookTriggers::new().every_nth_instruction(STEP), move |_, _| {
            let left = s.budget.get().saturating_sub(STEP as u64);
            s.budget.set(left);
            if left == 0 {
                Err(mlua::Error::runtime("the script ran for too long and was stopped"))
            } else {
                Ok(VmState::Continue)
            }
        })
        .map_err(|e| e.to_string())?;
        Ok(Host { lua, sh, scripts: Vec::new() })
    }

    /// Names of the loaded scripts.
    pub fn names(&self) -> &[String] {
        &self.scripts
    }

    fn start(&self, script: &str, now: u64, ctx: Option<&Ctx>) {
        self.sh.now.set(now);
        self.sh.budget.set(BUDGET);
        *self.sh.current.borrow_mut() = script.to_owned();
        if let Some(c) = ctx {
            *self.sh.ctx.borrow_mut() = c.clone();
        }
    }

    fn finish(&self) -> Vec<Action> {
        self.sh.current.borrow_mut().clear();
        std::mem::take(&mut *self.sh.out.borrow_mut())
    }

    fn error(&self, script: &str, e: mlua::Error) {
        let msg = e.to_string();
        let first = msg.lines().next().unwrap_or("error").to_owned();
        self.sh.out.borrow_mut().push(Action::Echo(format!("* [script {script}] error: {first}")));
    }

    /// Load a script from its text. Its registrations are tagged with `name`; loading a name again replaces it.
    pub fn load_source(&mut self, name: &str, source: &str, now: u64, ctx: &Ctx) -> Vec<Action> {
        self.unload_quiet(name);
        self.start(name, now, Some(ctx));
        let result = (|| -> mlua::Result<()> {
            // each script has its own globals, with the shared ones (chimera, string, ...) behind them
            let env = self.lua.create_table()?;
            let meta = self.lua.create_table()?;
            meta.set("__index", self.lua.globals())?;
            let _ = env.set_metatable(Some(meta));
            self.lua.load(source).set_name(format!("={name}")).set_environment(env).exec()
        })();
        match result {
            Ok(()) => self.scripts.push(name.to_owned()),
            Err(e) => {
                self.error(name, e);
                self.unload_quiet(name);
            }
        }
        self.finish()
    }

    fn unload_quiet(&mut self, name: &str) {
        self.scripts.retain(|s| s != name);
        let remove = |k: RegistryKey| {
            let _ = self.lua.remove_registry_value(k);
        };
        let (mut keep_h, mut gone_h) = (Vec::new(), Vec::new());
        for h in self.sh.handlers.borrow_mut().drain(..) {
            if h.script == name { gone_h.push(h.key) } else { keep_h.push(h) }
        }
        *self.sh.handlers.borrow_mut() = keep_h;
        let (mut keep_c, mut gone_c) = (Vec::new(), Vec::new());
        for c in self.sh.commands.borrow_mut().drain(..) {
            if c.script == name { gone_c.push(c.key) } else { keep_c.push(c) }
        }
        *self.sh.commands.borrow_mut() = keep_c;
        let (mut keep_t, mut gone_t) = (Vec::new(), Vec::new());
        for t in self.sh.timers.borrow_mut().drain(..) {
            if t.script == name { gone_t.push(t.key) } else { keep_t.push(t) }
        }
        *self.sh.timers.borrow_mut() = keep_t;
        for k in gone_h.into_iter().chain(gone_c).chain(gone_t) {
            remove(k);
        }
        self.sh.data.borrow_mut().remove(name);
    }

    /// Stop a script and forget what it registered. False if there was no such script.
    pub fn unload(&mut self, name: &str) -> bool {
        let had = self.scripts.iter().any(|s| s == name);
        self.unload_quiet(name);
        had
    }

    /// Commands scripts have added, for `/help` style listings.
    pub fn command_names(&self) -> Vec<String> {
        self.sh.commands.borrow().iter().map(|c| c.name.clone()).collect()
    }

    fn event_table(&self, fields: &[(&str, Val)]) -> mlua::Result<Table> {
        let t = self.lua.create_table()?;
        for (k, v) in fields {
            match v {
                Val::S(s) => t.set(*k, s.as_str())?,
                Val::B(b) => t.set(*k, *b)?,
            }
        }
        Ok(t)
    }

    /// Something happened (`message`, `notice`, `join`, `part`, `connect`, `disconnect`, ...): tell every script
    /// that asked about it.
    pub fn fire(&mut self, event: &str, fields: &[(&str, Val)], now: u64, ctx: &Ctx) -> Vec<Action> {
        let event = event.to_ascii_lowercase();
        let calls: Vec<(String, Function)> = self
            .sh
            .handlers
            .borrow()
            .iter()
            .filter(|h| h.event == event)
            .filter_map(|h| self.lua.registry_value::<Function>(&h.key).ok().map(|f| (h.script.clone(), f)))
            .collect();
        for (script, f) in calls {
            self.start(&script, now, Some(ctx));
            let r = self.event_table(fields).and_then(|t| f.call::<Value>(t));
            if let Err(e) = r {
                self.error(&script, e);
            }
        }
        self.finish()
    }

    /// A line the user typed (before it is run). `true` in the first part of the answer means a script handled it
    /// and it must not be sent or run.
    pub fn input(&mut self, line: &str, now: u64, ctx: &Ctx) -> (bool, Vec<Action>) {
        let calls: Vec<(String, Function)> = self
            .sh
            .handlers
            .borrow()
            .iter()
            .filter(|h| h.event == "input")
            .filter_map(|h| self.lua.registry_value::<Function>(&h.key).ok().map(|f| (h.script.clone(), f)))
            .collect();
        let mut swallowed = false;
        for (script, f) in calls {
            self.start(&script, now, Some(ctx));
            match f.call::<Value>(line.to_owned()) {
                Ok(Value::Boolean(true)) => {
                    swallowed = true;
                    break;
                }
                Ok(_) => {}
                Err(e) => self.error(&script, e),
            }
        }
        (swallowed, self.finish())
    }

    /// Run a script's `/command`. `None` when no script has that command.
    pub fn command(&mut self, name: &str, args: &str, now: u64, ctx: &Ctx) -> Option<Vec<Action>> {
        let name = name.to_ascii_lowercase();
        let found = {
            let cmds = self.sh.commands.borrow();
            cmds.iter().find(|c| c.name == name).and_then(|c| self.lua.registry_value::<Function>(&c.key).ok().map(|f| (c.script.clone(), f)))
        };
        let (script, f) = found?;
        self.start(&script, now, Some(ctx));
        if let Err(e) = f.call::<Value>(args.to_owned()) {
            self.error(&script, e);
        }
        Some(self.finish())
    }

    /// Time has passed: run the timers that are due.
    pub fn tick(&mut self, now: u64, ctx: &Ctx) -> Vec<Action> {
        let due: Vec<(u32, String, Function, bool)> = {
            let mut timers = self.sh.timers.borrow_mut();
            let mut due = Vec::new();
            for t in timers.iter_mut() {
                if t.due <= now {
                    if let Ok(f) = self.lua.registry_value::<Function>(&t.key) {
                        due.push((t.id, t.script.clone(), f, t.every.is_none()));
                    }
                    match t.every {
                        Some(ms) => t.due = now + ms,
                        None => t.due = u64::MAX,
                    }
                }
            }
            due
        };
        for (id, script, f, once) in due {
            self.start(&script, now, Some(ctx));
            if let Err(e) = f.call::<Value>(()) {
                self.error(&script, e);
            }
            if once {
                let mut timers = self.sh.timers.borrow_mut();
                if let Some(i) = timers.iter().position(|t| t.id == id) {
                    let t = timers.remove(i);
                    let _ = self.lua.remove_registry_value(t.key);
                }
            }
        }
        self.finish()
    }

    /// Is any timer waiting? (So the program need not wake up for nothing.)
    pub fn has_timers(&self) -> bool {
        !self.sh.timers.borrow().is_empty()
    }
}

/// The name a script file gets: its file name without `.lua`. `None` for anything that is not a plain `.lua` name.
pub fn script_name(file_name: &str) -> Option<String> {
    let stem = file_name.strip_suffix(".lua").or_else(|| file_name.strip_suffix(".LUA"))?;
    (!stem.is_empty() && stem.chars().all(|c| c.is_alphanumeric() || c == '_' || c == '-' || c == ' ' || c == '.')).then(|| stem.to_owned())
}

/// A sample script, written to the scripts folder the first time (as `example.lua.off`; remove `.off` to use it).
pub const EXAMPLE: &str = r#"-- ChimeraIRC example script. To use it, rename this file to example.lua and choose
-- Options > Scripts > Reload Scripts (or type /script reload).
--
-- What a script can do:
--   chimera.on("message", function(m) ... end)   m.nick  m.target  m.text  m.private  m.action  m.mine  m.network  m.msgid  m.reply_to
--   also "notice", "join", "part", "invite", "react", "connect", "disconnect" and "input" (return true to stop a typed line)
--   chimera.command("hello", function(args) ... end)   adds /hello
--   chimera.say(text)  chimera.me(text)  chimera.msg(target, text)  chimera.notice(target, text)
--   chimera.raw(line)  chimera.run("/any command")  chimera.echo(text) / print(text) show text only to you
--   chimera.timer(seconds, fn [, repeat])  chimera.cancel(id)
--   chimera.nick()  chimera.target()  chimera.network()
--   chimera.save(name, value)  chimera.load(name [, default])   small values kept between runs
-- Scripts cannot read or write files, use the network or start programs.

-- answer "!ping" in the channel it was said in
chimera.on("message", function(m)
  if not m.mine and not m.private and m.text:lower():find("!ping", 1, true) then
    chimera.say("pong, " .. m.nick)
  end
end)

-- /hello name
chimera.command("hello", function(args)
  chimera.echo("Hello, " .. (args ~= "" and args or "world") .. "! You are " .. chimera.nick() .. " on " .. chimera.network() .. ".")
end)
"#;

#[cfg(test)]
mod tests {
    use super::*;

    fn host() -> Host {
        Host::new(None).expect("lua starts")
    }

    fn ctx() -> Ctx {
        Ctx { nick: "me".into(), target: "#rust".into(), network: "net".into() }
    }

    fn load(h: &mut Host, src: &str) -> Vec<Action> {
        h.load_source("t", src, 0, &ctx())
    }

    fn echoes(a: &[Action]) -> Vec<String> {
        a.iter().filter_map(|x| if let Action::Echo(s) = x { Some(s.clone()) } else { None }).collect()
    }

    #[test]
    fn a_message_handler_answers_with_actions() {
        let mut h = host();
        let out = load(&mut h, r#"chimera.on("message", function(m) if m.text == "ping" and not m.mine then chimera.say("pong " .. m.nick) end end)"#);
        assert!(out.is_empty());
        let fields = [("nick", Val::from("bob")), ("text", Val::from("ping")), ("mine", Val::from(false))];
        assert_eq!(h.fire("message", &fields, 0, &ctx()), vec![Action::Run("/say pong bob".into())]);
        let mine = [("nick", Val::from("me")), ("text", Val::from("ping")), ("mine", Val::from(true))];
        assert!(h.fire("message", &mine, 0, &ctx()).is_empty());
        assert!(h.fire("join", &fields, 0, &ctx()).is_empty());
    }

    #[test]
    fn commands_and_the_context() {
        let mut h = host();
        load(&mut h, r#"chimera.command("who", function(a) chimera.echo(a .. ":" .. chimera.nick() .. "@" .. chimera.target() .. "/" .. chimera.network()) end)"#);
        assert_eq!(echoes(&h.command("WHO", "x", 0, &ctx()).unwrap()), ["x:me@#rust/net"]);
        assert!(h.command("nope", "", 0, &ctx()).is_none());
        assert_eq!(h.command_names(), ["who"]);
    }

    #[test]
    fn input_can_be_taken_over() {
        let mut h = host();
        load(&mut h, r#"chimera.on("input", function(line) if line == "secret" then chimera.echo("caught") return true end end)"#);
        let (swallowed, out) = h.input("secret", 0, &ctx());
        assert!(swallowed);
        assert_eq!(echoes(&out), ["caught"]);
        assert!(!h.input("other", 0, &ctx()).0);
    }

    #[test]
    fn timers_fire_once_or_repeat_and_can_be_cancelled() {
        let mut h = host();
        load(&mut h, r#"
            chimera.timer(1, function() chimera.echo("once") end)
            id = chimera.timer(2, function() chimera.echo("again") end, true)
            local gone = chimera.timer(1, function() chimera.echo("never") end)
            chimera.cancel(gone)
        "#);
        assert!(h.has_timers());
        assert!(h.tick(500, &ctx()).is_empty());
        assert_eq!(echoes(&h.tick(1000, &ctx())), ["once"]);
        assert_eq!(echoes(&h.tick(2000, &ctx())), ["again"]);
        assert!(h.tick(3000, &ctx()).is_empty());
        assert_eq!(echoes(&h.tick(4000, &ctx())), ["again"]);
    }

    #[test]
    fn scripts_cannot_reach_the_system() {
        let mut h = host();
        let out = load(&mut h, r#"
            local bad = {}
            for _, n in ipairs({"io", "os", "package", "debug", "dofile", "loadfile", "load", "require"}) do
              if _G[n] ~= nil then bad[#bad + 1] = n end
            end
            chimera.echo("open:" .. table.concat(bad, ","))
            chimera.echo(tostring(string.rep ~= nil) .. tostring(math.floor(2.5)) .. tostring(utf8.char(65)))
        "#);
        assert_eq!(echoes(&out), ["open:", "true2A"]);
    }

    #[test]
    fn a_loop_that_never_ends_is_stopped_and_reported() {
        let mut h = host();
        load(&mut h, r#"chimera.command("spin", function() while true do end end)"#);
        let out = h.command("spin", "", 0, &ctx()).unwrap();
        let e = echoes(&out);
        assert!(e.len() == 1 && e[0].contains("too long"), "{e:?}");
        // and the next call works again
        load(&mut h, r#"chimera.command("ok", function() chimera.echo("fine") end)"#);
        assert_eq!(echoes(&h.command("ok", "", 0, &ctx()).unwrap()), ["fine"]);
    }

    #[test]
    fn errors_are_reported_with_the_script_name_and_do_not_stop_others() {
        let mut h = host();
        let out = load(&mut h, "this is not lua");
        assert!(echoes(&out)[0].starts_with("* [script t] error:"));
        assert!(h.names().is_empty());
        h.load_source("a", r#"chimera.on("join", function() error("boom") end)"#, 0, &ctx());
        h.load_source("b", r#"chimera.on("join", function() chimera.echo("b ran") end)"#, 0, &ctx());
        let out = h.fire("join", &[], 0, &ctx());
        let e = echoes(&out);
        assert!(e[0].contains("[script a] error: ") && e[0].contains("boom"), "{e:?}");
        assert_eq!(e[1], "b ran");
    }

    #[test]
    fn scripts_have_their_own_globals_and_can_be_unloaded() {
        let mut h = host();
        h.load_source("a", "x = 1 chimera.on('join', function() chimera.echo('a' .. tostring(x)) end)", 0, &ctx());
        h.load_source("b", "chimera.on('join', function() chimera.echo('b' .. tostring(x)) end)", 0, &ctx());
        assert_eq!(echoes(&h.fire("join", &[], 0, &ctx())), ["a1", "bnil"]);
        assert!(h.unload("a"));
        assert!(!h.unload("a"));
        assert_eq!(echoes(&h.fire("join", &[], 0, &ctx())), ["bnil"]);
        // loading a name again replaces it rather than adding a second copy
        h.load_source("b", "chimera.on('join', function() chimera.echo('b2') end)", 0, &ctx());
        assert_eq!(echoes(&h.fire("join", &[], 0, &ctx())), ["b2"]);
        assert_eq!(h.names(), ["b"]);
    }

    #[test]
    fn text_sent_by_scripts_is_one_clean_line_and_talking_too_fast_is_cut_off() {
        let mut h = host();
        let out = load(&mut h, r#"chimera.msg("bob", "hi\r\nQUIT :x") chimera.raw("PRIVMSG a :b\nQUIT")"#);
        assert_eq!(out, vec![Action::Run("/msg bob hi  QUIT :x".into()), Action::Run("/raw PRIVMSG a :b QUIT".into())]);
        let out = h.load_source("t", r#"for i = 1, 100 do chimera.say("x" .. i) end"#, 5000, &ctx());
        let runs = out.iter().filter(|a| matches!(a, Action::Run(_))).count();
        assert_eq!(runs, RATE as usize);
        assert_eq!(echoes(&out).len(), 1);
        // a second later it may talk again
        let out = h.load_source("t", r#"chimera.say("again")"#, 7000, &ctx());
        assert_eq!(out, vec![Action::Run("/say again".into())]);
    }

    #[test]
    fn saved_values_survive_a_restart() {
        let dir = std::env::temp_dir().join(format!("chimerairc-script-test-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let mut h = Host::new(Some(&dir)).unwrap();
        h.load_source("s", r#"chimera.save("count", "7") chimera.save("note", "a\nb") chimera.save("tmp", "x") chimera.save("tmp", nil)"#, 0, &ctx());
        let mut h2 = Host::new(Some(&dir)).unwrap();
        let out = h2.load_source("s", r#"chimera.echo(chimera.load("count") .. "|" .. chimera.load("note") .. "|" .. tostring(chimera.load("tmp")) .. "|" .. chimera.load("none", "dflt"))"#, 0, &ctx());
        // (echo turns the line break into a space)
        assert_eq!(echoes(&out), ["7|a b|nil|dflt"]);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn script_file_names() {
        assert_eq!(script_name("greeter.lua"), Some("greeter".into()));
        assert_eq!(script_name("my script-2.lua"), Some("my script-2".into()));
        assert_eq!(script_name("example.lua.off"), None);
        assert_eq!(script_name(".lua"), None);
        assert_eq!(script_name("a/b.lua"), None);
        assert_eq!(script_name("notes.txt"), None);
    }

    #[test]
    fn the_example_script_loads_and_works() {
        let mut h = host();
        assert!(h.load_source("example", EXAMPLE, 0, &ctx()).is_empty());
        let f = [("nick", Val::from("bob")), ("text", Val::from("!PING")), ("mine", Val::from(false)), ("private", Val::from(false))];
        assert_eq!(h.fire("message", &f, 0, &ctx()), vec![Action::Run("/say pong, bob".into())]);
        assert_eq!(echoes(&h.command("hello", "Ann", 0, &ctx()).unwrap()), ["Hello, Ann! You are me on net."]);
    }
}
