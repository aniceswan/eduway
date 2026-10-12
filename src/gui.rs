//! Graphical frontend for eduway (`eduway gui`, needs `--features gui`).
//! Same engine as the CLI: login once, pick a tab/course, download everything.

use crate::api::{self, Client, Tab};
use crate::session;
use crate::vfs::Module;
use eframe::egui;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{channel, Receiver, Sender};
use std::sync::Arc;
use std::thread;

pub fn run() {
    let icon = eframe::icon_data::from_png_bytes(include_bytes!("../assets/icon-256.png"))
        .ok();
    let mut viewport = egui::ViewportBuilder::default().with_inner_size([980.0, 640.0]);
    if let Some(icon) = icon {
        viewport = viewport.with_icon(icon);
    }
    let opts = eframe::NativeOptions {
        viewport,
        ..Default::default()
    };
    if let Err(e) = eframe::run_native(
        "eduway",
        opts,
        Box::new(|cc| Ok(Box::new(App::new(cc.egui_ctx.clone())))),
    ) {
        eprintln!("GUI error: {e:?}");
    }
}

#[derive(Clone)]
struct CourseEntry {
    id: String,
    name: String,
}

#[derive(Clone)]
struct ModuleEntry {
    id: String,
    name: String,
    open: bool,
    activities: Vec<serde_json::Value>,
    items: Vec<ItemEntry>,
    loaded: bool,
}

#[derive(Clone)]
struct ItemEntry {
    name: String,
    ctype: String,
    size: Option<u64>,
    viewer: bool,
}

struct Job {
    label: String,
    url: String,
    dest: PathBuf,
    size: Option<u64>,
}

enum Msg {
    LoggedIn(String),
    LoginFail(String),
    Courses {
        tab: Tab,
        list: Vec<CourseEntry>,
    },
    Modules {
        course: usize,
        mods: Vec<ModuleEntry>,
    },
    Items {
        course: usize,
        module: usize,
        items: Vec<ItemEntry>,
    },
    DlProgress {
        done: usize,
        total: usize,
        file: String,
    },
    DlDone {
        ok: usize,
        skipped: usize,
        failed: Vec<String>,
        cancelled: bool,
    },
    Status(String),
}

fn sess_client() -> Option<Client> {
    session::load().and_then(|s| Client::new(s).ok())
}

struct App {
    client_ok: bool,
    status: String,
    user: String,
    tab: Tab,
    courses: Vec<CourseEntry>,
    sel_course: Option<usize>,
    modules: Vec<ModuleEntry>,
    mods_for: Option<usize>,
    busy: bool,
    done: usize,
    total: usize,
    current: String,
    log: Vec<String>,
    include_viewer: bool,
    cancel: Arc<AtomicBool>,
    search: String,
    styled: bool,
    logo: Option<egui::TextureHandle>,
    rx: Option<Receiver<Msg>>,
}

/// Brand accent (eduway blue) + helpers.
const ACCENT: egui::Color32 = egui::Color32::from_rgb(47, 92, 224);
const ACCENT_DARK: egui::Color32 = egui::Color32::from_rgb(30, 64, 175);
const GOOD: egui::Color32 = egui::Color32::from_rgb(74, 222, 128);
const BAD: egui::Color32 = egui::Color32::from_rgb(248, 113, 113);
const MUTED: egui::Color32 = egui::Color32::from_rgb(148, 163, 184);

/// Badge color per file type.
fn type_color(ctype: &str) -> egui::Color32 {
    match ctype {
        "pdf" => egui::Color32::from_rgb(248, 113, 113),
        "ppt" => egui::Color32::from_rgb(251, 146, 60),
        "doc" | "docx" => egui::Color32::from_rgb(96, 165, 250),
        "xls" | "xlsx" => egui::Color32::from_rgb(74, 222, 128),
        "video" | "mp4" => egui::Color32::from_rgb(192, 132, 252),
        "audio" | "mp3" => egui::Color32::from_rgb(45, 212, 191),
        "zip" | "rar" => egui::Color32::from_rgb(250, 204, 21),
        _ => MUTED,
    }
}

fn apply_style(ctx: &egui::Context) {
    let mut style = (*ctx.style()).clone();
    style.visuals.selection.bg_fill = ACCENT;
    style.visuals.hyperlink_color = egui::Color32::from_rgb(147, 197, 253);
    style.visuals.widgets.hovered.bg_fill = ACCENT_DARK;
    style.text_styles.insert(
        egui::TextStyle::Heading,
        egui::FontId::new(22.0, egui::FontFamily::Proportional),
    );
    style.text_styles.insert(
        egui::TextStyle::Body,
        egui::FontId::new(14.0, egui::FontFamily::Proportional),
    );
    style.spacing.item_spacing = egui::vec2(8.0, 6.0);
    style.spacing.button_padding = egui::vec2(10.0, 6.0);
    ctx.set_style(style);
}

impl App {
    fn new(ctx: egui::Context) -> Self {
        let (tx, rx) = channel();
        let mut app = Self {
            client_ok: false,
            status: String::new(),
            user: String::new(),
            tab: Tab::Active,
            courses: vec![],
            sel_course: None,
            modules: vec![],
            mods_for: None,
            busy: false,
            done: 0,
            total: 0,
            current: String::new(),
            log: vec![],
            include_viewer: false,
            cancel: Arc::new(AtomicBool::new(false)),
            search: String::new(),
            styled: false,
            logo: None,
            rx: Some(rx),
        };
        // auto-login from saved session
        if let Some(c) = sess_client() {
            match c.me() {
                Ok(me) => {
                    app.client_ok = true;
                    app.user = me
                        .get("name")
                        .or_else(|| me.get("email"))
                        .and_then(|x| x.as_str())
                        .unwrap_or("user")
                        .to_string();
                    app.status = format!("halo {}", app.user);
                    fetch_courses(tx, Tab::Active, ctx.clone());
                }
                Err(e) => app.status = format!("session mati: {e} — login ulang"),
            }
        }
        app
    }
}

impl App {
    fn sender(&mut self) -> Sender<Msg> {
        // each spawn creates its own channel pair stored back into self.rx
        let (tx, rx) = channel();
        self.rx = Some(rx);
        tx
    }

    /// Wipe session + caches and reset to the fresh login screen.
    /// Decode the bundled logo once; kept as a texture afterwards.
    fn logo(&mut self, ctx: &egui::Context) -> Option<egui::TextureHandle> {
        if self.logo.is_none() {
            let raw = include_bytes!("../assets/icon-256.png");
            if let Ok(img) = image::load_from_memory(raw) {
                let rgba = img.to_rgba8();
                let size = [rgba.width() as usize, rgba.height() as usize];
                let tex = ctx.load_texture(
                    "eduway-logo",
                    egui::ColorImage::from_rgba_unmultiplied(size, &rgba.into_raw()),
                    egui::TextureOptions::LINEAR,
                );
                self.logo = Some(tex);
            }
        }
        self.logo.clone()
    }

    fn logout(&mut self) {
        let _ = std::fs::remove_file(session::session_file());
        for tab in [Tab::Active, Tab::Inactive, Tab::All] {
            session::delete_courses(tab);
        }
        self.client_ok = false;
        self.user.clear();
        self.courses.clear();
        self.sel_course = None;
        self.modules.clear();
        self.mods_for = None;
        self.busy = false;
        self.done = 0;
        self.total = 0;
        self.current.clear();
        self.search.clear();
        self.status = "logout — sampai jumpa".to_string();
        self.log.push("logout: session + cache dihapus".to_string());
    }

    /// Login via embedded app window: run `eduway login --webview` as a
    /// child (it saves the session itself), then pick it up here.
    fn webview_login(tx: Sender<Msg>, ctx: egui::Context) {
        thread::spawn(move || {
            let exe = std::env::current_exe()
                .map(|p| p.to_string_lossy().to_string())
                .unwrap_or_else(|_| "eduway".to_string());
            let ok = std::process::Command::new(&exe)
                .args(["login", "--webview"])
                .status()
                .map(|s| s.success())
                .unwrap_or(false);
            if !ok {
                let _ = tx.send(Msg::LoginFail("jendela login dibatalkan/gagal".to_string()));
                return;
            }
            match session::load() {
                Some(s) => {
                    let name = Client::new(s)
                        .ok()
                        .and_then(|c| c.me().ok())
                        .and_then(|me| {
                            me.get("name")
                                .or_else(|| me.get("email"))
                                .and_then(|x| x.as_str())
                                .map(|x| x.to_string())
                        })
                        .unwrap_or_else(|| "user".to_string());
                    fetch_courses(tx.clone(), Tab::Active, ctx.clone());
                    let _ = tx.send(Msg::LoggedIn(name));
                }
                None => {
                    let _ = tx.send(Msg::LoginFail("session tidak tersimpan".to_string()));
                }
            }
        });
    }

    fn pump(&mut self, ctx: &egui::Context) {
        let mut again = false;
        if let Some(rx) = &self.rx {
            while let Ok(m) = rx.try_recv() {
                again = true;
                match m {
                    Msg::LoggedIn(u) => {
                        self.client_ok = true;
                        self.user = u.clone();
                        self.status = format!("halo {u}");
                        self.log.push(format!("login ok ({u})"));
                    }
                    Msg::LoginFail(e) => {
                        self.status = format!("login gagal: {e}");
                        self.log.push(format!("login FAIL: {e}"));
                    }
                    Msg::Courses { tab, list } => {
                        if tab == self.tab {
                            self.courses = list;
                            self.sel_course = None;
                            self.modules.clear();
                            self.status = format!("{} matkul [{}]", self.courses.len(), tab.dir());
                        }
                    }
                    Msg::Modules { course, mods } => {
                        if self.sel_course == Some(course) {
                            self.modules = mods;
                            self.mods_for = Some(course);
                        }
                    }
                    Msg::Items {
                        course,
                        module,
                        items,
                    } => {
                        if self.sel_course == Some(course) {
                            if let Some(m) = self.modules.get_mut(module) {
                                m.items = items;
                                m.loaded = true;
                            }
                        }
                    }
                    Msg::DlProgress { done, total, file } => {
                        self.done = done;
                        self.total = total;
                        self.current = file;
                    }
                    Msg::DlDone { ok, skipped, failed, cancelled } => {
                        self.busy = false;
                        self.current.clear();
                        self.status = if cancelled {
                            format!("dibatalkan — yang sudah selesai tetap kesimpan (ok={ok})")
                        } else {
                            format!("selesai: ok={ok} skipped={skipped} failed={}", failed.len())
                        };
                        self.log.push(format!(
                            "done ok={ok} skipped={skipped} failed={}",
                            failed.len()
                        ));
                        for f in failed {
                            self.log.push(format!("FAILED {f}"));
                        }
                    }
                    Msg::Status(s) => {
                        self.status = s.clone();
                        self.log.push(s);
                    }
                }
            }
        }
        if again || self.busy {
            ctx.request_repaint();
        }
    }
}

fn fetch_courses(tx: Sender<Msg>, tab: Tab, ctx: egui::Context) {
    thread::spawn(move || {
        let msg = match sess_client() {
            Some(c) => match c.list_all_courses(20, tab) {
                Ok(raw) => {
                    let mut list: Vec<CourseEntry> = raw
                        .into_iter()
                        .map(|v| {
                            let id = v.get("id").map(|x| {
                                if let Some(s) = x.as_str() {
                                    s.to_string()
                                } else {
                                    x.to_string()
                                }
                            });
                            let title = v
                                .get("name")
                                .and_then(|x| x.as_str())
                                .unwrap_or("course");
                            let code =
                                v.get("code").and_then(|x| x.as_str()).unwrap_or("");
                            let name = if code.is_empty() {
                                title.to_string()
                            } else {
                                format!("{code}-{title}")
                            };
                            (id, name)
                        })
                        .filter_map(|(id, name)| {
                            id.map(|id| CourseEntry { id, name })
                        })
                        .collect();
                    list.sort_by(|a, b| a.name.cmp(&b.name));
                    Msg::Courses { tab, list }
                }
                Err(e) => Msg::Status(format!("gagal load matkul: {e}")),
            },
            None => Msg::Status("belum login".to_string()),
        };
        let _ = tx.send(msg);
        ctx.request_repaint();
    });
}

fn fetch_modules(tx: Sender<Msg>, course: usize, id: String, ctx: egui::Context) {
    thread::spawn(move || {
        let msg = match sess_client() {
            Some(c) => match c.course_modules(&id) {
                Ok(mods) => Msg::Modules {
                    course,
                    mods: mods
                        .into_iter()
                        .map(|m| ModuleEntry {
                            id: m.id.clone(),
                            name: m.name.clone(),
                            open: m.is_open,
                            activities: m.activities.clone(),
                            items: vec![],
                            loaded: false,
                        })
                        .collect(),
                },
                Err(e) => Msg::Status(format!("gagal load modul: {e}")),
            },
            None => Msg::Status("belum login".to_string()),
        };
        let _ = tx.send(msg);
        ctx.request_repaint();
    });
}

fn fetch_items(tx: Sender<Msg>, course: usize, module: usize, m: Module, ctx: egui::Context) {
    thread::spawn(move || {
        let msg = match sess_client() {
            Some(c) => match c.module_items(&m) {
                Ok(items) => Msg::Items {
                    course,
                    module,
                    items: items
                        .into_iter()
                        .map(|it| {
                            let viewer = it.is_viewer_hint();
                            ItemEntry {
                                name: it.name,
                                ctype: it.ctype,
                                size: it.size,
                                viewer,
                            }
                        })
                        .collect(),
                },
                Err(e) => Msg::Status(format!("gagal load isi: {e}")),
            },
            None => Msg::Status("belum login".to_string()),
        };
        let _ = tx.send(msg);
        ctx.request_repaint();
    });
}

/// Collect (modules -> items -> urls) AND download, fully in background.
/// The UI thread never blocks, so the OS never reports "not responding".
/// `module_id`: None = whole course.
fn collect_and_download(
    tx: Sender<Msg>,
    ctx: egui::Context,
    course_id: String,
    course_name: String,
    module_id: Option<String>,
    include_viewer: bool,
    cancel: Arc<AtomicBool>,
) {
    thread::spawn(move || {
        let progress = |done: usize, total: usize, file: String| {
            let _ = tx.send(Msg::DlProgress { done, total, file });
            ctx.request_repaint();
        };
        let fail = |failed: Vec<String>| {
            let _ = tx.send(Msg::DlDone { ok: 0, skipped: 0, failed, cancelled: false });
            ctx.request_repaint();
        };
        let sess = match session::load() {
            Some(s) => s,
            None => {
                fail(vec!["belum login".to_string()]);
                return;
            }
        };
        let c = match Client::new(sess) {
            Ok(c) => c,
            Err(e) => {
                fail(vec![format!("{e:?}")]);
                return;
            }
        };
        let mods = match c.course_modules(&course_id) {
            Ok(m) => m,
            Err(e) => {
                fail(vec![format!("gagal load modul: {e}")]);
                return;
            }
        };
        let mods: Vec<_> = match &module_id {
            Some(mid) => mods.into_iter().filter(|m| &m.id == mid).collect(),
            None => mods,
        };
        if mods.is_empty() {
            fail(vec!["matkul/modul tidak ketemu".to_string()]);
            return;
        }
        let _ = tx.send(Msg::Status(format!(
            "memindai {} modul...",
            mods.len()
        )));
        let mut jobs = vec![];
        let mut skipped_viewer = 0usize;
        for (n, m) in mods.iter().enumerate() {
            if cancel.load(Ordering::Relaxed) {
                break;
            }
            let items = c.module_items(m).unwrap_or_default();
            for it in items {
                let is_viewer = it.is_viewer_hint();
                match Client::file_url(&it.raw) {
                    None => skipped_viewer += 1,
                    Some((url, fname, sz)) => {
                        if is_viewer && !include_viewer {
                            skipped_viewer += 1;
                            continue;
                        }
                        let safe: String = fname
                            .chars()
                            .map(|ch| {
                                if matches!(ch, '/' | '\\' | ':' | '*' | '?' | '"' | '<' | '>' | '|') {
                                    '_'
                                } else {
                                    ch
                                }
                            })
                            .collect();
                        let safe = safe.trim();
                        let safe = if safe.is_empty() { "file".to_string() } else { safe.to_string() };
                        let dest = crate::out_dir().join(&course_name).join(&m.name).join(&safe);
                        jobs.push(Job {
                            label: format!("{}/{}", m.name, safe),
                            url,
                            dest,
                            size: sz,
                        });
                    }
                }
            }
            let _ = tx.send(Msg::Status(format!(
                "memindai modul {}/{}...",
                n + 1,
                mods.len()
            )));
        }
        let total = jobs.len();
        let _ = tx.send(Msg::Status(format!(
            "{total} file, {skipped_viewer} viewer dilewati — mulai download"
        )));
        let (mut ok, mut skipped) = (0usize, 0usize);
        let mut failed = vec![];
        for (n, j) in jobs.iter().enumerate() {
            if cancel.load(Ordering::Relaxed) {
                let _ = tx.send(Msg::DlDone { ok, skipped, failed, cancelled: true });
                ctx.request_repaint();
                return;
            }
            progress(n, total, j.label.clone());
            match c.download(&j.url, &j.dest, j.size) {
                Ok(api::DlOutcome::SkipSameSize) => skipped += 1,
                Ok(api::DlOutcome::Done(_)) => ok += 1,
                Err(e) => failed.push(format!("{} :: {e}", j.label)),
            }
        }
        progress(total, total, String::new());
        let _ = tx.send(Msg::DlDone { ok, skipped, failed, cancelled: false });
        ctx.request_repaint();
    });
}

impl eframe::App for App {
    fn update(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
        self.pump(ctx);

        if !self.client_ok {
            if !self.styled {
                apply_style(ctx);
                self.styled = true;
            }
            egui::CentralPanel::default().show(ctx, |ui| {
                ui.vertical_centered(|ui| {
                    ui.add_space(24.0);
                    if let Some(tex) = self.logo(ctx) {
                        ui.add(egui::Image::from_texture((tex.id(), egui::vec2(96.0, 96.0))));
                    }
                    ui.add_space(4.0);
                    ui.label(
                        egui::RichText::new("eduway")
                            .size(40.0)
                            .strong()
                            .color(ACCENT),
                    );
                    ui.label(egui::RichText::new("download matkul Edunex tanpa ribet").size(15.0).color(MUTED));
                    ui.add_space(20.0);
                    egui::Frame::group(ui.style()).show(ui, |ui| {
                        ui.set_min_width(460.0);
                        ui.label(
                            egui::RichText::new("Login SSO sekali — sisanya otomatis.")
                                .size(16.0)
                                .strong(),
                        );
                        ui.add_space(4.0);
                        ui.label(
                            egui::RichText::new(
                                "Jendela login kebuka, kamu login seperti biasa, \
                                 token diambil + diverifikasi sendiri.",
                            )
                            .color(MUTED),
                        );
                        ui.add_space(10.0);
                        if ui
                            .add_sized(
                                [440.0, 44.0],
                                egui::Button::new(
                                    egui::RichText::new("Login dengan SSO").size(17.0).strong(),
                                )
                                .fill(ACCENT),
                            )
                            .clicked()
                        {
                            Self::webview_login(self.sender(), ctx.clone());
                        }
                    });
                    if !self.status.is_empty() {
                        ui.add_space(4.0);
                        ui.label(egui::RichText::new(&self.status).color(MUTED));
                    }
                });
            });
            return;
        }
        // top bar: brand + tabs + user
        egui::TopBottomPanel::top("tabs").show(ctx, |ui| {
            ui.horizontal(|ui| {
                if let Some(tex) = self.logo(ctx) {
                    ui.add(egui::Image::from_texture((tex.id(), egui::vec2(26.0, 26.0))));
                }
                ui.label(egui::RichText::new("eduway").size(18.0).strong().color(ACCENT));
                ui.separator();
                for tab in [Tab::Active, Tab::Inactive, Tab::All] {
                    if ui
                        .selectable_label(self.tab == tab, tab.dir())
                        .on_hover_text(tab.title())
                        .clicked()
                    {
                        self.tab = tab;
                        self.sel_course = None;
                        self.modules.clear();
                        self.search.clear();
                        fetch_courses(self.sender(), tab, ctx.clone());
                    }
                }
                if ui.small_button("refresh").clicked() {
                    fetch_courses(self.sender(), self.tab, ctx.clone());
                }
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    if ui.small_button("logout").on_hover_text("hapus session + cache").clicked() {
                        self.logout();
                    }
                    ui.label(
                        egui::RichText::new(&self.user)
                            .color(MUTED),
                    );
                });
            });
        });

        // bottom: progress + log
        egui::TopBottomPanel::bottom("status").show(ctx, |ui| {
            ui.horizontal(|ui| {
                if self.busy {
                    if self.total > 0 {
                        let p = self.done as f32 / self.total.max(1) as f32;
                        ui.add(
                            egui::ProgressBar::new(p)
                                .show_percentage()
                                .desired_width(220.0),
                        );
                        ui.label(
                            egui::RichText::new(format!("{}/{} {}", self.done, self.total, self.current))
                                .monospace(),
                        );
                    } else {
                        ui.spinner();
                        ui.label(&self.status);
                    }
                    if ui.small_button("Batal").on_hover_text("berhenti, yang sudah selesai tetap kesimpan").clicked() {
                        self.cancel.store(true, Ordering::Relaxed);
                        self.status = "membatalkan (tunggu file aktif selesai)...".to_string();
                    }
                } else {
                    let ok = self.status.starts_with("selesai") || self.status.starts_with("halo");
                    ui.label(egui::RichText::new(&self.status).color(if ok { GOOD } else { MUTED }));
                }
            });
            ui.separator();
            egui::ScrollArea::vertical().max_height(80.0).show(ui, |ui| {
                for l in self.log.iter().rev().take(50) {
                    let c = if l.contains("FAIL") || l.contains("gagal") {
                        BAD
                    } else if l.starts_with("done") || l.starts_with("ok") {
                        GOOD
                    } else {
                        MUTED
                    };
                    ui.label(egui::RichText::new(l).monospace().small().color(c));
                }
            });
        });

        // left: search + courses
        let mut pick = None;
        egui::SidePanel::left("courses").show(ctx, |ui| {
            ui.heading(format!("{} matkul", self.tab.dir()));
            ui.add(
                egui::TextEdit::singleline(&mut self.search)
                    .hint_text("cari matkul…")
                    .desired_width(f32::INFINITY),
            );
            ui.separator();
            let q = self.search.trim().to_lowercase();
            egui::ScrollArea::vertical().show(ui, |ui| {
                for (i, c) in self.courses.iter().enumerate() {
                    if !q.is_empty() && !c.name.to_lowercase().contains(&q) {
                        continue;
                    }
                    let sel = self.sel_course == Some(i);
                    if ui
                        .selectable_label(sel, egui::RichText::new(&c.name).strong())
                        .clicked()
                    {
                        pick = Some(i);
                    }
                }
                if self.courses.is_empty() {
                    ui.label(egui::RichText::new("loading…").color(MUTED));
                }
            });
        });
        if let Some(i) = pick {
            self.sel_course = Some(i);
            self.modules.clear();
            self.mods_for = None;
            let cid = self.courses.get(i).map(|c| c.id.clone());
            if let Some(cid) = cid {
                fetch_modules(self.sender(), i, cid, ctx.clone());
            }
        }

        // center: modules + download
        egui::CentralPanel::default().show(ctx, |ui| {
            let Some(ci) = self.sel_course else {
                ui.centered_and_justified(|ui| {
                    ui.label(egui::RichText::new("pilih matkul di kiri").size(16.0).color(MUTED));
                });
                return;
            };
            let Some(course) = self.courses.get(ci).cloned() else {
                return;
            };
            ui.horizontal(|ui| {
                ui.heading(&course.name);
            });
            ui.horizontal(|ui| {
                if ui
                    .add_sized(
                        [280.0, 34.0],
                        egui::Button::new("Download 1 matkul ini").fill(ACCENT),
                    )
                    .on_hover_text("semua modul + semua file matkul ini")
                    .clicked()
                {
                    self.download_course(ci, None, ctx.clone());
                }
                ui.checkbox(&mut self.include_viewer, "ikutkan viewer");
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    ui.label(
                        egui::RichText::new(format!("ke {}", crate::out_dir().display()))
                            .small()
                            .color(MUTED),
                    );
                });
            });
            ui.separator();
            egui::ScrollArea::vertical().show(ui, |ui| {
                for (mi, m) in self.modules.clone().iter().enumerate() {
                    let title = if m.loaded {
                        format!("{}  ({} file)", m.name, m.items.len())
                    } else {
                        m.name.clone()
                    };
                    let icon = if !m.open { "[tutup] " } else { "" };
                    egui::CollapsingHeader::new(egui::RichText::new(format!("{icon}{title}")).strong())
                        .default_open(false)
                        .show(ui, |ui| {
                            ui.horizontal(|ui| {
                                if !m.loaded {
                                    if ui.small_button("buka isi").clicked() {
                                        self.load_module_items(ci, mi, ctx.clone());
                                    }
                                } else if ui.small_button("download modul ini").clicked() {
                                    self.download_course(ci, Some(mi), ctx.clone());
                                }
                            });
                            for it in &m.items {
                                ui.horizontal(|ui| {
                                    ui.label("•");
                                    ui.label(&it.name);
                                    if !it.ctype.is_empty() {
                                        ui.label(
                                            egui::RichText::new(format!("[{}]", it.ctype))
                                                .small()
                                                .strong()
                                                .color(type_color(it.ctype.as_str())),
                                        );
                                    }
                                    if let Some(s) = it.size {
                                        ui.label(
                                            egui::RichText::new(crate::api::fmt_size(Some(s)))
                                                .small()
                                                .color(MUTED),
                                        );
                                    }
                                    if it.viewer {
                                        ui.label(egui::RichText::new("[viewer]").small().color(BAD));
                                    }
                                });
                            }
                        });
                }
                if self.modules.is_empty() {
                    ui.label(egui::RichText::new("loading modul…").color(MUTED));
                }
            });
        });
    }
}

impl App {
    fn load_module_items(&mut self, ci: usize, mi: usize, ctx: egui::Context) {
        // reuse the already-loaded module: no network on the UI thread
        // (fetch_items runs its HTTP in a worker thread).
        if let Some(e) = self.modules.get(mi).cloned() {
            let m = crate::vfs::Module {
                id: e.id,
                name: e.name,
                is_open: e.open,
                activities: e.activities,
                items: vec![],
                items_fetched: false,
            };
            fetch_items(self.sender(), ci, mi, m, ctx.clone());
        }
    }

    /// Start a course/module download. Returns immediately (no freeze):
    /// everything happens in `collect_and_download` on a worker thread.
    fn download_course(&mut self, ci: usize, module: Option<usize>, ctx: egui::Context) {
        let course = match self.courses.get(ci).cloned() {
            Some(c) => c,
            None => return,
        };
        let module_id = module.and_then(|mi| self.modules.get(mi).map(|m| m.id.clone()));
        if module.is_some() && module_id.is_none() {
            self.status = "buka dulu modulnya, baru download".to_string();
            return;
        }
        self.cancel.store(false, Ordering::Relaxed);
        self.busy = true;
        self.done = 0;
        self.total = 0;
        self.current.clear();
        self.status = "menyiapkan daftar file...".to_string();
        self.log.push(format!("dl {} -> {}", course.name, crate::out_dir().display()));
        ctx.request_repaint();
        collect_and_download(
            self.sender(),
            ctx,
            course.id,
            course.name,
            module_id,
            self.include_viewer,
            self.cancel.clone(),
        );
    }


}
