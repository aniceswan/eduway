mod api;
#[cfg(feature = "gui")]
mod gui;
mod blogin;
mod session;
mod vfs;
mod wlogin;

use anyhow::Result;
use rustyline::completion::{Completer, Pair};
use rustyline::error::ReadlineError;
use rustyline::highlight::Highlighter;
use rustyline::hint::Hinter;
use rustyline::validate::Validator;
use rustyline::{Context, Helper};
use serde_json::Value;
use std::collections::{HashMap, HashSet};
use std::path::PathBuf;

type Rl = rustyline::Editor<EduHelper, rustyline::history::DefaultHistory>;

/// Tab-completion: first word = commands, rest = paths/flags.
#[derive(Default)]
struct EduHelper {
    cmds: Vec<String>,
    paths: Vec<String>,
}

impl EduHelper {
    fn suggest(&self, line: &str, pos: usize) -> (usize, Vec<Pair>) {
        let line = &line[..pos.min(line.len())];
        let start = line
            .rfind(|c: char| c.is_whitespace())
            .map(|i| i + 1)
            .unwrap_or(0);
        let word = &line[start..];
        let head = line[..start].split_whitespace().next().unwrap_or("");
        let first = head.is_empty();
        if !first && (word.starts_with("--") || word == "-") {
            let flags = match head {
                "dl" | "dlall" => vec!["--ext=", "--include-viewer"],
                "refresh" => vec!["--all"],
                _ => vec![],
            };
            let out: Vec<Pair> = flags
                .into_iter()
                .filter(|f| f.starts_with(word))
                .map(|f| Pair {
                    display: f.to_string(),
                    replacement: f.to_string(),
                })
                .collect();
            return (start, out);
        }
        let pool = if first { &self.cmds } else { &self.paths };
        let mut out = vec![];
        for c in pool {
            if c.starts_with(word) && c.as_str() != word {
                out.push(Pair {
                    display: c.clone(),
                    replacement: c.clone(),
                });
            }
        }
        out.sort_by(|a, b| a.replacement.cmp(&b.replacement));
        (start, out)
    }
}

impl Completer for EduHelper {
    type Candidate = Pair;
    fn complete(
        &self,
        line: &str,
        pos: usize,
        _ctx: &Context<'_>,
    ) -> rustyline::Result<(usize, Vec<Pair>)> {
        Ok(self.suggest(line, pos))
    }
}

impl Hinter for EduHelper {
    type Hint = String;
}

impl Highlighter for EduHelper {}

impl Validator for EduHelper {}

impl Helper for EduHelper {}

/// Rebuild completion candidates from current shell position.
/// Network-free: only names already loaded in memory.
fn refresh_completions(rl: &mut Rl, shell: &Shell) {
    let h = match rl.helper_mut() {
        Some(h) => h,
        None => return,
    };
    h.cmds = [
        "login", "ls", "cd", "pwd", "info", "dl", "dlall", "get", "whoami", "status", "help",
        "refresh", "exit", "quit",
    ]
    .iter()
    .map(|s| s.to_string())
    .collect();
    let mut p = vec![
        "active/".to_string(),
        "inactive/".to_string(),
        "all/".to_string(),
        "../".to_string(),
        "/".to_string(),
    ];
    if let Some(tab) = shell.loaded_tab {
        for ck in shell.vfs.courses.keys() {
            p.push(format!("{ck}/"));
            p.push(format!("{}/{ck}/", tab.dir()));
        }
        if let Some(c) = shell.vfs.current_course() {
            for m in &c.modules {
                let ms = slug(&m.name, 120);
                p.push(format!("{ms}/"));
                if let Some(cur) = shell.vfs.current_module() {
                    if cur.id == m.id {
                        for it in &m.items {
                            p.push(it.label());
                        }
                    }
                }
            }
        }
    }
    h.paths = p;
}

use api::{fmt_size, ApiFail, Client, Tab};
use vfs::{slug, Course, Item, Vfs};

/// Download root: `~/Downloads/eduway` when that folder exists,
/// else `./downloads`. Override with `EDUWAY_DIR`.
pub(crate) fn out_dir() -> PathBuf {
    if let Ok(env) = std::env::var("EDUWAY_DIR") {
        if !env.trim().is_empty() {
            return PathBuf::from(env);
        }
    }
    if let Some(home) = dirs::home_dir() {
        let dl = home.join("Downloads");
        if dl.is_dir() {
            return dl.join("eduway");
        }
    }
    PathBuf::from("downloads")
}

struct Shell {
    vfs: Vfs,
    client: Option<Client>,
    loaded_tab: Option<Tab>,
}

struct Job {
    rel: Vec<String>,
    url: String,
    size: Option<u64>,
}

struct Skip {
    course: String,
    module: String,
    item: Item,
    has_meta: bool,
}

impl Shell {
    fn need_auth(&self) -> bool {
        if self.client.is_none() {
            println!("not logged in. run `login`.");
            return true;
        }
        false
    }

    fn client(&self) -> &Client {
        self.client.as_ref().unwrap()
    }

    fn prompt(&self) -> String {
        let pos = self.vfs.pwd();
        if pos == "/" {
            "eduway> ".to_string()
        } else {
            format!("eduway:{}> ", pos.trim_start_matches('/'))
        }
    }

    fn fill_courses(&mut self, courses: Vec<Value>) {
        self.vfs.courses.clear();
        for c in courses {
            let mut co = Course::new(c.clone());
            let mut base = co.name.clone();
            let mut i = 2;
            while self.vfs.courses.contains_key(&base) {
                base = format!("{}-{i}", co.name);
                i += 1;
            }
            co.name = base.clone();
            co.raw = c;
            self.vfs.courses.insert(base, co);
        }
    }

    /// Tab for the current position (default: active).
    fn tab_for_pos(&self) -> Tab {
        self.vfs.current_tab().unwrap_or(Tab::Active)
    }

    fn ensure_courses(&mut self, force: bool, tab: Tab) -> Result<(), ApiFail> {
        if self.loaded_tab == Some(tab) && !force {
            return Ok(());
        }
        if !force {
            if let Some(cached) = session::load_courses(tab) {
                let n = cached.len();
                self.fill_courses(cached);
                self.loaded_tab = Some(tab);
                println!("loaded {n} courses [{}] (cache)", tab.dir());
                return Ok(());
            }
        }
        println!("loading courses [{}]...", tab.dir());
        let t0 = std::time::Instant::now();
        let courses = self.client().list_all_courses(20, tab)?;
        println!(
            "loaded {} courses [{}] in {:.1}s",
            courses.len(),
            tab.dir(),
            t0.elapsed().as_secs_f64()
        );
        session::save_courses(tab, &courses);
        self.fill_courses(courses);
        self.loaded_tab = Some(tab);
        Ok(())
    }

    fn ensure_modules(&mut self, course_key: &str) -> Result<(), ApiFail> {
        let done = self
            .vfs
            .courses
            .get(course_key)
            .map(|c| !c.modules.is_empty() || c.detail_fetched)
            .unwrap_or(true);
        if done {
            return Ok(());
        }
        let cid = match self.vfs.courses.get(course_key) {
            Some(c) => c.id.clone(),
            None => return Ok(()),
        };
        println!("loading modules for {course_key}...");
        let out = self.client().course_modules(&cid)?;
        if let Some(c) = self.vfs.courses.get_mut(course_key) {
            c.modules = out;
            c.detail_fetched = true;
        }
        Ok(())
    }

    fn ensure_items(&mut self, course_key: &str, module_id: &str) -> Result<(), ApiFail> {
        let done = self
            .vfs
            .courses
            .get(course_key)
            .and_then(|c| c.modules.iter().find(|m| m.id == module_id))
            .map(|m| !m.items.is_empty() || m.items_fetched)
            .unwrap_or(true);
        if done {
            return Ok(());
        }
        let mname = self
            .vfs
            .courses
            .get(course_key)
            .and_then(|c| c.modules.iter().find(|m| m.id == module_id))
            .map(|m| m.name.clone())
            .unwrap_or_default();
        println!("loading contents for {mname}...");
        let module_snapshot = self
            .vfs
            .courses
            .get(course_key)
            .and_then(|c| c.modules.iter().find(|m| m.id == module_id))
            .cloned();
        let new_items = match module_snapshot {
            Some(m) => self.client().module_items(&m)?,
            None => vec![],
        };
        if let Some(c) = self.vfs.courses.get_mut(course_key) {
            if let Some(m) = c.modules.iter_mut().find(|m| m.id == module_id) {
                m.items = new_items;
                m.items_fetched = true;
            }
        }
        Ok(())
    }

    fn ensure_module_at_pos(&mut self) -> Result<(), ApiFail> {
        if self.vfs.pos.len() < 2 {
            return Ok(());
        }
        let tab = self.tab_for_pos();
        self.ensure_courses(false, tab)?;
        let ck = self.vfs.pos[1].clone();
        self.ensure_modules(&ck)?;
        if self.vfs.pos.len() >= 3 {
            let mid = self
                .vfs
                .courses
                .get(&ck)
                .and_then(|c| {
                    c.modules
                        .iter()
                        .find(|m| slug(&m.name, 120) == self.vfs.pos[2])
                })
                .map(|m| m.id.clone());
            if let Some(mid) = mid {
                self.ensure_items(&ck, &mid)?;
            }
        }
        Ok(())
    }

    // ---------------- login ----------------
    fn do_login(&mut self, arg: &str, rl: &mut Rl) {
        let arg = arg.trim();
        if arg.is_empty() || arg == "--webview" {
            // default: automatic window login, no manual steps
            crate::wlogin::run();
        }
        if arg == "--browser" {
            self.do_login_browser();
            return;
        }
        if arg == "--webview" {
            crate::wlogin::run();
        }
        if !arg.is_empty() && !arg.starts_with('{') && arg.split_whitespace().count() == 2 {
            let mut it = arg.splitn(2, char::is_whitespace);
            let user = it.next().unwrap_or("").to_string();
            let pwd = it.next().unwrap_or("").to_string();
            let anon = match Client::anonymous() {
                Ok(c) => c,
                Err(e) => {
                    println!("FAIL {e:?}");
                    return;
                }
            };
            match anon.login_password(&user, &pwd) {
                Ok(sess) => match Client::new(sess.clone()) {
                    Ok(c) => match c.me() {
                        Ok(_) => {
                            if session::save(&sess).is_err() {
                                println!("FAIL cannot save session");
                                return;
                            }
                            self.client = Some(Client::new(sess).unwrap());
                            self.loaded_tab = None;
                            println!("ok, logged in via password");
                        }
                        Err(e) => println!("FAIL {e}"),
                    },
                    Err(e) => println!("FAIL {e:?}"),
                },
                Err(e) => println!("FAIL {e}"),
            }
            return;
        }
        let raw = if arg.is_empty() {
            match rl.readline("paste session (copy(localStorage.getItem('auth'))) or file path: ") {
                Ok(l) => l.trim().to_string(),
                Err(_) => {
                    println!();
                    return;
                }
            }
        } else {
            arg.to_string()
        };
        let s = raw.trim().trim_matches('"').trim_matches('\'').to_string();
        let mut from_file: Option<std::path::PathBuf> = None;
        if s.len() < 1000 && !s.contains('\n') {
            let p = PathBuf::from(&s);
            if std::fs::metadata(&p).map(|m| m.is_file()).unwrap_or(false) {
                from_file = Some(p);
            }
        }
        let parsed: Value = if let Some(p) = from_file {
            match std::fs::read_to_string(&p) {
                Ok(t) => match serde_json::from_str(&t) {
                    Ok(v) => v,
                    Err(_) => {
                        println!("FAIL bad file JSON");
                        return;
                    }
                },
                Err(e) => {
                    println!("FAIL bad file: {e}");
                    return;
                }
            }
        } else {
            match serde_json::from_str(&raw) {
                Ok(v) => v,
                Err(_) => {
                    println!("FAIL not JSON and not a file. F12 Console: copy(localStorage.getItem('auth'))");
                    return;
                }
            }
        };
        let sess = match session::normalize(&parsed) {
            Some(x) => x,
            None => {
                println!("FAIL JSON needs accessToken/access_token/token.");
                return;
            }
        };
        println!("verifying...");
        let c = match Client::new(sess.clone()) {
            Ok(c) => c,
            Err(e) => {
                println!("FAIL {e:?}");
                return;
            }
        };
        let name = match c.me() {
            Ok(me) => me
                .get("name")
                .or_else(|| me.get("email"))
                .or_else(|| me.get("username"))
                .and_then(|x| x.as_str())
                .unwrap_or("")
                .to_string(),
            Err(e) => {
                println!("FAIL {e}");
                return;
            }
        };
        if session::save(&sess).is_err() {
            println!("FAIL cannot save session");
            return;
        }
        self.client = Some(Client::new(sess).unwrap());
        self.loaded_tab = None;
        let show = if name.is_empty() { "user".to_string() } else { name };
        println!("ok, halo {show} (ketik `ls` untuk daftar matkul)");
    }

    /// `login --browser`: open Edunex in the browser, user logs in, one
    /// bookmarklet click sends the session back here. Blocks up to 5 min.
    fn do_login_browser(&mut self) {
        use crate::blogin::Event;
        println!("membuka browser ke halaman login edunex...");
        println!("(kalau tidak terbuka, buka manual: https://edunex.itb.ac.id/pages/login)");
        crate::blogin::run(crate::blogin::TIMEOUT, true, |ev| match ev {
            Event::Ready { bookmarklet, page } => {
                println!("1. login di browser sampai dashboard kebuka");
                println!("2. buat bookmark baru, isi URL-nya persis dengan baris ini:");
                println!("{bookmarklet}");
                println!("3. klik bookmark itu di tab edunex yang sudah login");
                println!("   status: {page}");
                println!("menunggu (maks 5 menit, Ctrl-C batal)...");
            }
            Event::Done(Ok(sess)) => {
                if session::save(&sess).is_err() {
                    println!("FAIL cannot save session");
                    return;
                }
                match Client::new(sess) {
                    Ok(c) => {
                        self.client = Some(c);
                        self.loaded_tab = None;
                        println!("ok, halo (ketik `ls` untuk daftar matkul)");
                    }
                    Err(e) => println!("FAIL {e:?}"),
                }
            }
            Event::Done(Err(e)) => println!("FAIL {e}"),
        });
    }

    // ---------------- ls ----------------
    fn do_ls(&mut self, arg: &str) {
        if self.need_auth() {
            return;
        }
        // root lists the My Courses tabs, no fetch needed
        if self.vfs.pos.is_empty() && arg.trim().is_empty() {
            for n in self.vfs.ls() {
                println!("{n}");
            }
            return;
        }
        if let Err(e) = self.ensure_module_at_pos() {
            println!("FAIL {e}");
            return;
        }
        let target = arg.trim();
        let saved = self.vfs.pos.clone();
        // `ls inactive` / `ls /active/x` from anywhere: resolve from root
        if !target.is_empty() {
            let first = target.trim_start_matches('/').split('/').next().unwrap_or("");
            if Tab::parse(first).is_some() {
                self.vfs.pos = vec![];
            }
        }
        if let Err(e) = self.ensure_module_at_pos() {
            println!("FAIL {e}");
            self.vfs.pos = saved;
            return;
        }
        let pos = if target.is_empty() {
            self.vfs.pos.clone()
        } else {
            match self.resolve_deep(target) {
                Ok(p) => p,
                Err(e) => {
                    println!("FAIL {e}");
                    self.vfs.pos = saved;
                    return;
                }
            }
        };
        self.vfs.pos = pos;
        if let Err(e) = self.ensure_module_at_pos() {
            println!("FAIL {e}");
            self.vfs.pos = saved;
            return;
        }
        for n in self.vfs.ls() {
            println!("{n}");
        }
        self.vfs.pos = saved;
    }

    // ---------------- cd ----------------
    fn do_cd(&mut self, arg: &str) {
        if self.need_auth() {
            return;
        }
        let arg = if arg.trim().is_empty() { "/" } else { arg.trim() };
        let saved = self.vfs.pos.clone();
        // `cd inactive` / `cd /active/x` from anywhere: resolve from root
        let first = arg.trim_start_matches('/').split('/').next().unwrap_or("");
        if Tab::parse(first).is_some() {
            self.vfs.pos = vec![];
            let tab = Tab::parse(first).unwrap();
            let _ = self.ensure_courses(false, tab);
        } else if !self.vfs.pos.is_empty() {
            let tab = self.tab_for_pos();
            let _ = self.ensure_courses(false, tab);
            if self.vfs.pos.len() >= 3 {
                let ck = self.vfs.pos[1].clone();
                let _ = self.ensure_modules(&ck);
            }
        }
        let newpos = match self.resolve_deep(arg) {
            Ok(p) => p,
            Err(e) => {
                self.vfs.pos = saved;
                println!("FAIL {e}");
                return;
            }
        };
        if !newpos.is_empty() {
            let tab = Tab::parse(&newpos[0]).unwrap_or(Tab::Active);
            if self.ensure_courses(false, tab).is_err() {
                // resolve already succeeded; modules just won't prefetch
            }
        }
        if newpos.len() >= 2 {
            if !self.vfs.courses.contains_key(&newpos[1]) {
                println!("FAIL no such path: {arg}");
                return;
            }
            let _ = self.ensure_modules(&newpos[1]);
        }
        self.vfs.pos = newpos;
    }

    // ---------------- info ----------------
    fn do_info(&mut self, arg: &str) {
        if self.need_auth() {
            return;
        }
        let target = arg.trim();
        if target.is_empty() {
            println!("pos: {}", self.vfs.pwd());
            if self.vfs.pos.len() == 1 {
                let tab = self.tab_for_pos();
                println!("tab: {} ({})", tab.dir(), tab.title());
            }
            if let Some(c) = self.vfs.current_course() {
                let t = c.raw.get("name").and_then(|x| x.as_str()).unwrap_or("");
                println!("course id={} name={t}", c.id);
            }
            if let Some(m) = self.vfs.current_module() {
                println!(
                    "module id={} name={} open={} items={}",
                    m.id,
                    m.name,
                    m.is_open,
                    m.items.len()
                );
            }
            return;
        }
        // resolving inside a tab may need that tab's courses first
        if !self.vfs.pos.is_empty() {
            let tab = self.tab_for_pos();
            let _ = self.ensure_courses(false, tab);
        }
        let np = match self.resolve_deep(target) {
            Ok(p) => p,
            Err(e) => {
                println!("FAIL {e}");
                return;
            }
        };
        match np.len() {
            0 => println!("root: my courses tabs (active/ inactive/ all/)"),
            1 => {
                let tab = Tab::parse(&np[0]).unwrap_or(Tab::Active);
                if let Err(e) = self.ensure_courses(false, tab) {
                    println!("FAIL {e}");
                    return;
                }
                println!(
                    "tab: {} ({}) — {} courses",
                    tab.dir(),
                    tab.title(),
                    self.vfs.courses.len()
                );
            }
            2 => {
                let tab = Tab::parse(&np[0]).unwrap_or(Tab::Active);
                if let Err(e) = self.ensure_courses(false, tab) {
                    println!("FAIL {e}");
                    return;
                }
                if let Some(c) = self.vfs.courses.get(&np[1]) {
                    let t = c.raw.get("name").and_then(|x| x.as_str()).unwrap_or("");
                    println!("id: {}\ntitle: {t}", c.id);
                } else {
                    println!("FAIL not found");
                }
            }
            3 => {
                let tab = Tab::parse(&np[0]).unwrap_or(Tab::Active);
                if let Err(e) = self.ensure_courses(false, tab) {
                    println!("FAIL {e}");
                    return;
                }
                let _ = self.ensure_modules(&np[1]);
                let c = match self.vfs.courses.get(&np[1]) {
                    Some(c) => c,
                    None => {
                        println!("FAIL not found");
                        return;
                    }
                };
                for m in &c.modules {
                    if slug(&m.name, 120) == np[2] {
                        println!(
                            "module id={} name={} open={} activities={} items={}",
                            m.id,
                            m.name,
                            m.is_open,
                            m.activities.len(),
                            m.items.len()
                        );
                        return;
                    }
                }
                println!("FAIL not found");
            }
            _ => {
                let tab = Tab::parse(&np[0]).unwrap_or(Tab::Active);
                if let Err(e) = self.ensure_courses(false, tab) {
                    println!("FAIL {e}");
                    return;
                }
                let ck = np[1].clone();
                let _ = self.ensure_modules(&ck);
                let mid = self
                    .vfs
                    .courses
                    .get(&ck)
                    .and_then(|c| {
                        c.modules
                            .iter()
                            .find(|m| slug(&m.name, 120) == np[2])
                            .map(|m| m.id.clone())
                    });
                if let Some(mid) = mid {
                    let _ = self.ensure_items(&ck, &mid);
                }
                if let Some(c) = self.vfs.courses.get(&ck) {
                    for m in &c.modules {
                        if slug(&m.name, 120) != np[2] {
                            continue;
                        }
                        for it in &m.items {
                            if it.label() == np[3] || it.name == np[3] {
                                let (url, sz) = match Client::file_url(&it.raw) {
                                    Some((u, _, s)) => (u, s.or(it.size)),
                                    None => ("(none)".to_string(), it.size),
                                };
                                println!(
                                    "title: {}\ntype: {}\nsize: {}\nviewer: {}\nurl: {}",
                                    it.name,
                                    if it.ctype.is_empty() { "?" } else { &it.ctype },
                                    fmt_size(sz),
                                    if it.is_viewer_hint() { "yes" } else { "no" },
                                    url
                                );
                                return;
                            }
                        }
                    }
                }
                println!("FAIL not found");
            }
        }
    }

    /// Resolve with staged loading: if the full path fails, resolve its
    /// parent, load that course's modules, then retry. This makes
    /// `cd active/MATKUL/MODUL` work in one step.
    fn resolve_deep(&mut self, path: &str) -> Result<Vec<String>, String> {
        if let Ok(p) = self.vfs.resolve(path) {
            return Ok(p);
        }
        let trimmed = path.trim_end_matches('/');
        let parent = match trimmed.rsplit_once('/') {
            Some((par, _)) if !par.is_empty() => par,
            _ => return self.vfs.resolve(path),
        };
        let ppos = self.vfs.resolve(parent)?;
        if ppos.len() == 2 {
            let ck = ppos[1].clone();
            let _ = self.ensure_modules(&ck);
        }
        self.vfs.resolve(path)
    }

    /// Best-effort preload so resolve() can see tab/course names.
    fn ensure_for_target(&mut self, target: Option<&str>) {
        if let Some(t) = target {
            let first = t.trim_start_matches('/').split('/').next().unwrap_or("");
            if !first.is_empty() && first != "." && first != ".." && first != "~" {
                if let Some(tab) = Tab::parse(first) {
                    let _ = self.ensure_courses(false, tab);
                    return;
                }
            }
        }
        if !self.vfs.pos.is_empty() {
            let tab = self.tab_for_pos();
            let _ = self.ensure_courses(false, tab);
        }
    }

    // ---------------- collect ----------------
    fn collect(&mut self, include_viewer: bool, exts: &HashSet<String>) -> (Vec<Job>, Vec<Skip>) {
        let mut jobs = vec![];
        let mut skips = vec![];
        let mut seen: HashMap<String, HashSet<String>> = HashMap::new();
        // root `dl` means the active tab (same default as the sidebar)
        let tab = self.tab_for_pos();
        if self.loaded_tab != Some(tab) {
            if self.ensure_courses(false, tab).is_err() {
                return (vec![], vec![]);
            }
        }
        let keys: Vec<String> = if self.vfs.pos.len() < 2 {
            self.vfs.courses.keys().cloned().collect()
        } else {
            vec![self.vfs.pos[1].clone()]
        };
        for ck in keys {
            if self.ensure_modules(&ck).is_err() {
                continue;
            }
            let wanted: Option<String> = if self.vfs.pos.len() == 3 {
                Some(self.vfs.pos[2].clone())
            } else {
                None
            };
            let mids: Vec<(String, String)> = match self.vfs.courses.get(&ck) {
                Some(c) => c
                    .modules
                    .iter()
                    .filter(|m| wanted.as_ref().map(|w| slug(&m.name, 120) == *w).unwrap_or(true))
                    .map(|m| (m.id.clone(), slug(&m.name, 120)))
                    .collect(),
                None => vec![],
            };
            let course_name = self
                .vfs
                .courses
                .get(&ck)
                .map(|c| c.name.clone())
                .unwrap_or_default();
            for (mid, mp) in mids {
                let _ = self.ensure_items(&ck, &mid);
                let items = match self.vfs.courses.get(&ck) {
                    Some(c) => c
                        .modules
                        .iter()
                        .find(|m| m.id == mid)
                        .map(|m| m.items.clone())
                        .unwrap_or_default(),
                    None => vec![],
                };
                for it in items {
                    let f = Client::file_url(&it.raw);
                    let viewer = f.is_none() || it.is_viewer_hint();
                    if viewer && !include_viewer {
                        skips.push(Skip {
                            course: course_name.clone(),
                            module: mp.clone(),
                            item: it,
                            has_meta: false,
                        });
                        continue;
                    }
                    match f {
                        None => skips.push(Skip {
                            course: course_name.clone(),
                            module: mp.clone(),
                            item: it,
                            has_meta: true,
                        }),
                        Some((url, fname, sz)) => {
                            if !exts.is_empty() {
                                let e = fname
                                    .rsplit('.')
                                    .next()
                                    .unwrap_or("")
                                    .to_lowercase();
                                if !exts.contains(&e) {
                                    continue;
                                }
                            }
                            let dir: Vec<String> = if self.vfs.pos.len() < 2 {
                                vec![course_name.clone(), mp.clone()]
                            } else if self.vfs.pos.len() == 2 {
                                vec![mp.clone()]
                            } else {
                                vec![]
                            };
                            let mut fname = slug(&fname, 120);
                            let key = dir.join("/");
                            let set = seen.entry(key).or_default();
                            if !set.insert(fname.to_lowercase()) {
                                let short: String =
                                    it.id.chars().take(6).collect();
                                fname = match fname.rfind('.') {
                                    Some(i) => format!(
                                        "{} ({}){}",
                                        &fname[..i],
                                        short,
                                        &fname[i..]
                                    ),
                                    None => format!("{fname} ({short})"),
                                };
                                set.insert(fname.to_lowercase());
                            }
                            let mut rel = dir;
                            rel.push(fname);
                            jobs.push(Job {
                                rel,
                                url,
                                size: sz,
                            });
                        }
                    }
                }
            }
        }
        (jobs, skips)
    }

    // ---------------- dl ----------------
    /// Shared `--ext / --include-viewer / [path]` parsing for `dl` and `dlall`.
    fn parse_dl_args(arg: &str) -> Result<(HashSet<String>, bool, Option<String>), String> {
        let mut exts = HashSet::new();
        let mut include_viewer = false;
        let mut target: Option<String> = None;
        let toks: Vec<&str> = arg.split_whitespace().collect();
        let mut i = 0;
        while i < toks.len() {
            let t = toks[i];
            if t == "--include-viewer" {
                include_viewer = true;
            } else if t == "--ext" {
                if i + 1 < toks.len() {
                    for e in toks[i + 1].split(',') {
                        let e = e.trim().trim_start_matches('.').to_lowercase();
                        if !e.is_empty() {
                            exts.insert(e);
                        }
                    }
                    i += 1;
                }
            } else if let Some(v) = t.strip_prefix("--ext=") {
                for e in v.split(',') {
                    let e = e.trim().trim_start_matches('.').to_lowercase();
                    if !e.is_empty() {
                        exts.insert(e);
                    }
                }
            } else if t.starts_with("--") {
                return Err(format!("unknown flag {t}"));
            } else if target.is_some() {
                return Err("only one path allowed".to_string());
            } else {
                target = Some(t.to_string());
            }
            i += 1;
        }
        Ok((exts, include_viewer, target))
    }

    fn do_dl(&mut self, arg: &str) {
        if self.need_auth() {
            return;
        }
        let (exts, include_viewer, target) = match Self::parse_dl_args(arg) {
            Ok(x) => x,
            Err(e) => {
                println!("FAIL {e}");
                return;
            }
        };
        self.ensure_for_target(target.as_deref());
        let old = self.vfs.pos.clone();
        if let Some(t) = target {
            // `dl inactive/...` from anywhere: resolve from root
            let first = t.trim_start_matches('/').split('/').next().unwrap_or("");
            if Tab::parse(first).is_some() {
                self.vfs.pos = vec![];
            }
            match self.resolve_deep(&t) {
                Ok(p) => self.vfs.pos = p,
                Err(e) => {
                    println!("FAIL {e}");
                    self.vfs.pos = old;
                    return;
                }
            }
        }
        self.run_download(include_viewer, &exts);
        self.vfs.pos = old;
    }

    /// `dlall <course-path>` — download a whole course (all its modules)
    /// from anywhere, without `cd`. Accepts `matkul`, `active/matkul`,
    /// or `/inactive/matkul`. Same flags as `dl`.
    fn do_dlall(&mut self, arg: &str) {
        if self.need_auth() {
            return;
        }
        let (exts, include_viewer, target) = match Self::parse_dl_args(arg) {
            Ok(x) => x,
            Err(e) => {
                println!("FAIL {e}");
                return;
            }
        };
        let t = match target {
            Some(t) => t,
            None => {
                println!("usage: dlall <course>  (contoh: dlall active/MA1101-aljabar-linear)");
                return;
            }
        };
        let old = self.vfs.pos.clone();
        self.ensure_for_target(Some(&t));
        // `dlall inactive/MA1101-x` from anywhere: resolve from root
        let first = t.trim_start_matches('/').split('/').next().unwrap_or("");
        if Tab::parse(first).is_some() {
            self.vfs.pos = vec![];
        }
        // bare course name: search every tab so `dlall MA1101-x` just works
        let mut pos = match self.resolve_deep(&t) {
            Ok(p) => p,
            Err(_) => match self.resolve_course_any_tab(&t) {
                Some(p) => p,
                None => {
                    self.vfs.pos = old;
                    println!("FAIL no such course: {t}");
                    return;
                }
            },
        };
        if pos.len() > 2 {
            self.vfs.pos = old;
            println!("FAIL dlall needs a course, not a module/file (pakai `dl` di dalam modul)");
            return;
        }
        if pos.len() < 1 {
            self.vfs.pos = old;
            println!("FAIL dlall needs a course");
            return;
        }
        // make sure the tab's courses are really loaded before collecting
        let tab = Tab::parse(&pos[0]).unwrap_or(Tab::Active);
        if let Err(e) = self.ensure_courses(false, tab) {
            self.vfs.pos = old;
            println!("FAIL {e}");
            return;
        }
        // re-resolve: a bare name found via another tab must point there
        if pos.len() == 2 && !self.vfs.courses.contains_key(&pos[1]) {
            match self.resolve_course_any_tab(&t) {
                Some(p) => pos = p,
                None => {
                    self.vfs.pos = old;
                    println!("FAIL no such course: {t}");
                    return;
                }
            }
        }
        self.vfs.pos = pos;
        self.run_download(include_viewer, &exts);
        self.vfs.pos = old;
    }

    /// Find `name` among all tabs' courses. Returns the position.
    fn resolve_course_any_tab(&mut self, name: &str) -> Option<Vec<String>> {
        let bare = name
            .trim_start_matches('/')
            .rsplit('/')
            .next()
            .unwrap_or(name)
            .trim_end_matches('/');
        if bare.is_empty() {
            return None;
        }
        for tab in [Tab::Active, Tab::Inactive, Tab::All] {
            if self.ensure_courses(false, tab).is_err() {
                continue;
            }
            if self.vfs.courses.contains_key(bare) {
                return Some(vec![tab.dir().to_string(), bare.to_string()]);
            }
            // fuzzy: contains-match, first hit wins
            let hit = self
                .vfs
                .courses
                .keys()
                .find(|k| k.to_lowercase().contains(&bare.to_lowercase()))
                .cloned();
            if let Some(h) = hit {
                return Some(vec![tab.dir().to_string(), h]);
            }
        }
        None
    }

    /// Download everything under the current position (must be set by caller).
    fn run_download(&mut self, include_viewer: bool, exts: &HashSet<String>) {
        // local tree skips the tab level: downloads/<course>/<module>/<file>
        let at_top = self.vfs.pos.len() < 2;
        let (jobs, skips) = self.collect(include_viewer, &exts);
        let total_size: u64 = jobs.iter().filter_map(|j| j.size).sum();
        println!(
            "{} file, ~{}, {} viewer-only dilewati",
            jobs.len(),
            fmt_size(Some(total_size)),
            skips.iter().filter(|s| !s.has_meta).count()
        );
        let base: PathBuf = if at_top {
            out_dir()
        } else {
            out_dir().join(self.vfs.pos.iter().skip(1).map(|x| slug(x, 120)).collect::<Vec<_>>().join("/"))
        };
        // NOTE: collect() already prefixes course/module dirs; when at_root base=downloads,
        // else base=downloads/<pos...> and rel is relative to pos. Both correct.
        let (mut ok, mut skipped, mut failed) = (0, 0, 0);
        let mut fails: Vec<(String, String)> = vec![];
        for (n, job) in jobs.iter().enumerate() {
            let dest = job.rel.iter().fold(base.clone(), |p, x| p.join(x));
            let name = dest
                .file_name()
                .map(|x| x.to_string_lossy().to_string())
                .unwrap_or_default();
            let tag = format!("[{}/{}] ", n + 1, jobs.len());
            if let Some(e) = job.size {
                if dest.exists() && std::fs::metadata(&dest).map(|m| m.len()).unwrap_or(0) == e {
                    println!("{tag}{name} ... SKIP sudah ada (ukuran sama)");
                    skipped += 1;
                    continue;
                }
            }
            match self.client().download(&job.url, &dest, job.size) {
                Ok(api::DlOutcome::SkipSameSize) => {
                    println!("{tag}{name} ... SKIP sudah ada (ukuran sama)");
                    skipped += 1;
                }
                Ok(api::DlOutcome::Done(final_dest)) => {
                    let shown = final_dest
                        .file_name()
                        .map(|x| x.to_string_lossy().to_string())
                        .unwrap_or(name);
                    let sz = final_dest
                        .metadata()
                        .map(|m| m.len())
                        .unwrap_or(job.size.unwrap_or(0));
                    println!("{tag}{shown} ... ok ({})", fmt_size(Some(sz)));
                    ok += 1;
                }
                Err(e) => {
                    let m = format!("{e}");
                    let cut: String = m.chars().take(160).collect();
                    println!("{tag}{name} ... FAIL {cut}");
                    failed += 1;
                    fails.push((dest.to_string_lossy().to_string(), cut));
                }
            }
        }
        for s in &skips {
            if !s.has_meta {
                println!(
                    "SKIP viewer-only (tidak ada file): {} / {} / {} (lihat `dl --include-viewer`)",
                    s.course, s.module, s.item.name
                );
                skipped += 1;
            } else {
                let mdest = base
                    .join(if at_top { s.course.clone() } else { String::new() })
                    .join(&s.module)
                    .join(format!("{}.txt", slug(&s.item.name, 120)));
                if let Some(p) = mdest.parent() {
                    let _ = std::fs::create_dir_all(p);
                }
                let body = format!(
                    "title: {}\ntype: {}\ncontent:\n{}\n",
                    s.item.name,
                    s.item.ctype,
                    s.item.content.chars().take(4000).collect::<String>()
                );
                match std::fs::write(&mdest, body) {
                    Ok(_) => {
                        println!(
                            "META viewer saved: {}",
                            mdest.file_name().map(|x| x.to_string_lossy().to_string()).unwrap_or_default()
                        );
                        ok += 1;
                    }
                    Err(e) => {
                        println!("FAIL meta {}: {e}", s.item.name);
                        failed += 1;
                    }
                }
            }
        }
        println!("done: ok={ok} skipped={skipped} failed={failed}");
        for (d, e) in fails {
            println!("FAILED {d} :: {e}");
        }
    }

    // ---------------- get ----------------
    fn do_get(&mut self, arg: &str) {
        if self.need_auth() {
            return;
        }
        let name = arg.trim().trim_matches('"').trim();
        if name.is_empty() {
            println!("usage: get <file>");
            return;
        }
        if self.vfs.pos.len() < 3 {
            println!("FAIL get works inside a module dir (cd active/<matkul>/<modul> dulu)");
            return;
        }
        let ck = self.vfs.pos[1].clone();
        if let Err(e) = self.ensure_modules(&ck) {
            println!("FAIL {e}");
            return;
        }
        let mid = match self.vfs.courses.get(&ck).and_then(|c| {
            c.modules
                .iter()
                .find(|m| slug(&m.name, 120) == self.vfs.pos[2])
                .map(|m| m.id.clone())
        }) {
            Some(m) => m,
            None => {
                println!("FAIL not found");
                return;
            }
        };
        if let Err(e) = self.ensure_items(&ck, &mid) {
            println!("FAIL {e}");
            return;
        }
        let hit = self
            .vfs
            .courses
            .get(&ck)
            .and_then(|c| c.modules.iter().find(|m| m.id == mid))
            .and_then(|m| {
                m.items
                    .iter()
                    .find(|it| it.label() == name || it.name == name)
                    .cloned()
            });
        match hit {
            None => println!("FAIL no such file: {name}"),
            Some(it) => match Client::file_url(&it.raw) {
                None => println!("SKIP viewer-only (lihat `dl --include-viewer`)"),
                Some((url, fname, sz)) => {
                    let dest = out_dir()
                        .join(self.vfs.pos.iter().skip(1).map(|x| slug(x, 120)).collect::<Vec<_>>().join("/"))
                        .join(slug(&fname, 120));
                    match self.client().download(&url, &dest, sz) {
                        Ok(api::DlOutcome::SkipSameSize) => {
                            println!("ok (SKIP sudah ada, ukuran sama)");
                        }
                        Ok(api::DlOutcome::Done(final_dest)) => {
                            let s = final_dest.metadata().map(|m| m.len()).unwrap_or(0);
                            let shown = final_dest
                                .file_name()
                                .map(|x| x.to_string_lossy().to_string())
                                .unwrap_or_default();
                            println!("ok: {shown} ({})", fmt_size(Some(s)));
                        }
                        Err(e) => println!("FAIL {e}"),
                    }
                }
            },
        }
    }

    /// `refresh` — drop cache and refetch the current tab (or `--all` tabs).
    fn do_refresh(&mut self, arg: &str) {
        if self.need_auth() {
            return;
        }
        if arg.trim() == "--all" {
            for tab in [Tab::Active, Tab::Inactive, Tab::All] {
                session::delete_courses(tab);
            }
            self.loaded_tab = None;
            println!("cache cleared (all tabs). run `ls`.");
            return;
        }
        let tab = self.tab_for_pos();
        session::delete_courses(tab);
        self.loaded_tab = None;
        match self.ensure_courses(false, tab) {
            Ok(_) => println!("refreshed [{}].", tab.dir()),
            Err(e) => println!("FAIL {e}"),
        }
    }

    /// `logout` — wipe session + caches (fresh start).
    fn do_logout(&mut self) {
        let _ = std::fs::remove_file(session::session_file());
        for tab in [Tab::Active, Tab::Inactive, Tab::All] {
            session::delete_courses(tab);
        }
        self.client = None;
        self.loaded_tab = None;
        self.vfs = Vfs::default();
        println!("logout — session + cache dihapus.");
    }

    fn do_whoami(&mut self) {
        match self.client.as_ref() {
            None => println!("not logged in"),
            Some(_) => {
                if let Some(s) = session::load() {
                    println!("session: {}", session::describe(&s));
                }
                match self.client().me() {
                    Ok(me) => {
                        let s = serde_json::to_string_pretty(&me).unwrap_or_default();
                        let cut: String = s.chars().take(1500).collect();
                        println!("{cut}");
                    }
                    Err(e) => println!("FAIL {e}"),
                }
            }
        }
    }
}

fn print_help(topic: &str) {
    let all = [
        ("login", "buka jendela login SSO (otomatis)"),
        ("ls", "[path?] list courses/modules/files. viewer files show [viewer]"),
        ("cd", "<path|..|/|~> move. typo prints similar names"),
        ("pwd", "print position"),
        ("info", "[path?] detail: title, type, size, url, viewer or not"),
        ("dl", "[--ext=pdf,pptx] [--include-viewer] [path?] download all here, recursive"),
        ("dlall", "<course> download a whole course (all modules) from anywhere"),
        ("get", "<file> download one file here"),
        ("whoami", "user + session age (alias: status)"),
        ("logout", "wipe session + caches"),
        ("refresh", "refetch course list (`refresh --all` = all tabs)"),
        #[cfg(feature = "gui")]
        ("gui", "open the graphical app"),
        ("help", "[cmd] this help"),
        ("exit", "quit (quit, Ctrl-D too)"),
    ];
    if topic.is_empty() {
        println!("commands:");
        for (c, d) in all {
            println!("  {c:<8} {d}");
        }
    } else {
        match all.iter().find(|(c, _)| *c == topic) {
            Some((c, d)) => println!("{c}: {d}"),
            None => println!("no help for {topic}"),
        }
    }
}

fn main() -> Result<()> {
    let mut shell = Shell {
        vfs: Vfs::default(),
        client: session::load().and_then(|s| Client::new(s).ok()),
        loaded_tab: None,
    };
    let mut rl: Rl = Rl::new()?;
    rl.set_helper(Some(EduHelper::default()));
    println!("eduway. type help. (login first)");
    // one-shot mode: eduway ls / eduway dl ...
    let args: Vec<String> = std::env::args().skip(1).collect();
    if !args.is_empty() {
        let line = args.join(" ");
        dispatch(&mut shell, &line, &mut rl);
        return Ok(());
    }
    loop {
        let prompt = shell.prompt();
        refresh_completions(&mut rl, &shell);
        match rl.readline(&prompt) {
            Ok(line) => {
                let line = line.trim();
                if line.is_empty() {
                    continue;
                }
                let _ = rl.add_history_entry(line);
                if !dispatch(&mut shell, line, &mut rl) {
                    break;
                }
            }
            Err(ReadlineError::Interrupted | ReadlineError::Eof) => {
                println!();
                break;
            }
            Err(e) => {
                println!("FAIL {e:?}");
                break;
            }
        }
    }
    Ok(())
}

/// Returns false when the shell should exit.
fn dispatch(shell: &mut Shell, line: &str, rl: &mut Rl) -> bool {
    let (cmd, rest) = match line.find(char::is_whitespace) {
        Some(i) => (&line[..i], line[i..].trim()),
        None => (line, ""),
    };
    match cmd {
        "login" => shell.do_login(rest, rl),
        "ls" => shell.do_ls(rest),
        "cd" => shell.do_cd(rest),
        "pwd" => println!("{}", shell.vfs.pwd()),
        "info" => shell.do_info(rest),
        "dl" => shell.do_dl(rest),
        "dlall" => shell.do_dlall(rest),
        "get" => shell.do_get(rest),
        "whoami" | "status" => shell.do_whoami(),
        "logout" => shell.do_logout(),
        "refresh" => shell.do_refresh(rest),
        "gui" => {
            #[cfg(feature = "gui")]
            {
                crate::gui::run();
            }
            #[cfg(not(feature = "gui"))]
            {
                println!("GUI belum di-build. Build: cargo build --release --features gui");
            }
        }
        "help" | "?" => print_help(rest),
        "exit" | "quit" => return false,
        "" => {}
        _ => println!("unknown: {cmd} (try help)"),
    }
    true
}

#[cfg(test)]
mod helper_tests {
    use super::*;

    fn helper() -> EduHelper {
        EduHelper {
            cmds: vec!["ls".into(), "login".into(), "dlall".into()],
            paths: vec!["active/".into(), "MA1101-x/".into(), "Minggu-01/".into()],
        }
    }

    fn reps(pairs: Vec<Pair>) -> Vec<String> {
        pairs.into_iter().map(|p| p.replacement).collect()
    }

    #[test]
    fn completes_command() {
        let (_, c) = helper().suggest("l", 1);
        assert_eq!(reps(c), vec!["login".to_string(), "ls".to_string()]);
    }

    #[test]
    fn completes_path() {
        let (_, c) = helper().suggest("cd MA", 5);
        assert!(reps(c).contains(&"MA1101-x/".to_string()));
    }

    #[test]
    fn completes_flag() {
        let (_, c) = helper().suggest("dl --", 5);
        let r = reps(c);
        assert!(r.contains(&"--ext=".to_string()));
        assert!(r.contains(&"--include-viewer".to_string()));
    }

    #[test]
    fn no_self_match() {
        let (_, c) = helper().suggest("ls", 2);
        assert!(c.is_empty());
    }
}
