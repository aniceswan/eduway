use anyhow::{Context, Result};
use reqwest::blocking::{Client as Http, Response};
use reqwest::StatusCode;
use serde_json::Value;
use std::collections::HashMap;
use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::time::Duration;

use crate::session::Session;
use crate::vfs::is_viewer_type;
use crate::vfs::{Item, Module};

pub const API_BASE: &str = "https://api-edunex.cognisia.id";
pub const STORAGE_DL: &str = "https://api-edunex.cognisia.id/storage/files/download?file=";
pub const CDN_BASE: &str = "https://cdn-edunex.itb.ac.id";
pub const BROWSER_UA: &str = "Mozilla/5.0 (X11; Linux x86_64) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/120.0 Safari/537.36";
const TIMEOUT: Duration = Duration::from_secs(60);
const POLITE_DELAY: Duration = Duration::from_millis(700);

/// My Courses tabs, exactly like the sidebar page.
/// `qs` values are copied from the `StudentCourseList` bundle
/// (`course/list-lecturer`: Active `[is_active][is]=1`,
/// Inactive `[is_active][is]=0`, All = no filter).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Tab {
    Active,
    Inactive,
    All,
}

impl Tab {
    pub fn dir(&self) -> &'static str {
        match self {
            Tab::Active => "active",
            Tab::Inactive => "inactive",
            Tab::All => "all",
        }
    }

    pub fn title(&self) -> &'static str {
        match self {
            Tab::Active => "Active Courses",
            Tab::Inactive => "Inactive Courses",
            Tab::All => "All Courses",
        }
    }

    pub fn qs(&self) -> Option<(&'static str, &'static str)> {
        match self {
            Tab::Active => Some(("filter[is_active][is]", "1")),
            Tab::Inactive => Some(("filter[is_active][is]", "0")),
            Tab::All => None,
        }
    }

    pub fn parse(s: &str) -> Option<Tab> {
        match s.trim().to_lowercase().as_str() {
            "active" | "active-courses" | "activecourses" => Some(Tab::Active),
            "inactive" | "inactive-courses" | "inactivecourses" => Some(Tab::Inactive),
            "all" | "all-courses" | "allcourses" => Some(Tab::All),
            _ => None,
        }
    }
}

#[derive(Debug)]
pub enum ApiFail {
    AuthExpired(String),
    Http(u16, String),
    Net(String),
}

impl std::fmt::Display for ApiFail {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::AuthExpired(m) => write!(f, "{m}"),
            Self::Http(s, m) => write!(f, "HTTP {s}: {m}"),
            Self::Net(m) => write!(f, "{m}"),
        }
    }
}

pub struct Client {
    http: Http,
    sess: Session,
}

impl Client {
    pub fn new(sess: Session) -> Result<Self> {
        let http = Http::builder()
            .timeout(TIMEOUT)
            .user_agent(&sess.user_agent)
            .build()
            .context("build http client")?;
        Ok(Self { http, sess })
    }

    pub fn anonymous() -> Result<Self> {
        Self::new(Session {
            user_agent: BROWSER_UA.to_string(),
            ..Default::default()
        })
    }

    fn url(&self, path_or_url: &str) -> String {
        if path_or_url.starts_with("http") {
            path_or_url.to_string()
        } else {
            format!("{API_BASE}{path_or_url}")
        }
    }

    fn send(
        &self,
        method: reqwest::Method,
        path_or_url: &str,
        query: Option<&[(&str, String)]>,
        body: Option<&Value>,
    ) -> Result<Response, ApiFail> {
        let url = self.url(path_or_url);
        let mut last: Option<String> = None;
        for attempt in 0..3 {
            let mut req = self
                .http
                .request(method.clone(), &url)
                .header("Accept", "application/json")
                .header("Content-Type", "application/json")
                .header("Authorization", self.sess.bearer());
            if let Some(q) = query {
                req = req.query(q);
            }
            if let Some(b) = body {
                req = req.json(b);
            }
            match req.send() {
                Err(e) => {
                    last = Some(e.to_string());
                    std::thread::sleep(POLITE_DELAY * (attempt + 1));
                    continue;
                }
                Ok(r) => {
                    let st = r.status();
                    if st == StatusCode::UNAUTHORIZED || st == StatusCode::FORBIDDEN {
                        return Err(ApiFail::AuthExpired(
                            "session expired (401/403), run `login` again".to_string(),
                        ));
                    }
                    if st.is_server_error() {
                        last = Some(format!("server error {st}"));
                        std::thread::sleep(POLITE_DELAY * (attempt + 1));
                        continue;
                    }
                    if st.is_client_error() || !st.is_success() {
                        let code = st.as_u16();
                        let text = r.text().unwrap_or_default();
                        let cut: String = text.chars().take(500).collect();
                        return Err(ApiFail::Http(code, cut));
                    }
                    std::thread::sleep(POLITE_DELAY);
                    return Ok(r);
                }
            }
        }
        Err(ApiFail::Net(
            last.unwrap_or_else(|| "network failed after retries".to_string()),
        ))
    }

    fn get_json(
        &self,
        path: &str,
        query: Option<&[(&str, String)]>,
    ) -> Result<Value, ApiFail> {
        let r = self.send(reqwest::Method::GET, path, query, None)?;
        let body: Value = r.json().unwrap_or(Value::Null);
        Ok(deser(&body))
    }

    // ---------- auth ----------
    pub fn login_password(&self, username: &str, password: &str) -> Result<Session, ApiFail> {
        let body = serde_json::json!({"username": username, "password": password});
        let r = self.send(reqwest::Method::POST, "/user/auth/login", None, Some(&body))?;
        let v: Value = r.json().unwrap_or(Value::Null);
        let data = v.get("data").unwrap_or(&v);
        let at = data
            .get("access_token")
            .and_then(|x| x.as_str())
            .unwrap_or("");
        if at.is_empty() {
            let cut = serde_json::to_string(&v).unwrap_or_default();
            let cut: String = cut.chars().take(300).collect();
            return Err(ApiFail::Http(200, format!("login has no access_token: {cut}")));
        }
        Ok(Session {
            access_token: at.to_string(),
            refresh_token: data
                .get("refresh_token")
                .and_then(|x| x.as_str())
                .map(|s| s.to_string()),
            expiration: data.get("expires_in").map(|x| x.to_string()),
            user_agent: self.sess.user_agent.clone(),
        })
    }

    pub fn me(&self) -> Result<Value, ApiFail> {
        self.get_json("/login/me", None)
    }

    // ---------- courses (My Courses tabs) ----------
    //
    // Same endpoint the website's My Courses page (`StudentCourseList`)
    // uses: `course/fetchCourses` -> GET /course/courses with the tab's
    // `qs` filter and `include=lecturer,lecturer.user`, 20 per page.
    pub fn list_courses(
        &self,
        limit: u32,
        offset: u32,
        tab: Tab,
    ) -> Result<(Vec<Value>, Option<u64>), ApiFail> {
        let mut q = vec![
            ("include".to_string(), "lecturer,lecturer.user".to_string()),
            ("page[limit]".to_string(), limit.to_string()),
            ("page[offset]".to_string(), offset.to_string()),
        ];
        if let Some((k, v)) = tab.qs() {
            q.push((k.to_string(), v.to_string()));
        }
        let qr: Vec<(&str, String)> =
            q.iter().map(|(k, v)| (k.as_str(), v.clone())).collect();
        let body = self.get_json("/course/courses", Some(&qr))?;
        let total = body.get("meta").and_then(|m| m.get("total")).and_then(|t| t.as_u64());
        let items = match body {
            Value::Array(a) => a,
            Value::Object(mut m) => match m.remove("data") {
                Some(Value::Array(a)) => a,
                Some(v) => vec![v],
                None => vec![],
            },
            _ => vec![],
        };
        Ok((items, total))
    }

    pub fn list_all_courses(&self, limit: u32, tab: Tab) -> Result<Vec<Value>, ApiFail> {
        let mut out: Vec<Value> = vec![];
        let mut off = 0u32;
        for page_no in 1..=10 {
            let (page, total) = self.list_courses(limit, off, tab)?;
            eprintln!(
                "  [{}] page {page_no}: {} items (server total: {})",
                tab.dir(),
                page.len(),
                total.map(|t| t.to_string()).unwrap_or_else(|| "?".to_string())
            );
            if page.is_empty() {
                break;
            }
            let old: std::collections::HashSet<String> = out.iter().map(item_key).collect();
            let fresh: Vec<Value> = page
                .iter()
                .filter(|x| !old.contains(&item_key(x)))
                .cloned()
                .collect();
            let full_page = page.len() as u32 >= limit;
            out.extend(fresh.clone());
            if !full_page || fresh.is_empty() {
                break;
            }
            off += limit;
        }
        Ok(out)
    }

    pub fn get_course(&self, course_id: &str) -> Result<Value, ApiFail> {
        let q = [(
            "include",
            "modules,modules.activities,lecturer,lecturer.user,assistant,class".to_string(),
        )];
        self.get_json(&format!("/course/courses/{course_id}"), Some(&q))
    }

    // ---------- contents ----------
    pub fn list_contents(
        &self,
        contentable_id: &str,
        contentable_type: &str,
    ) -> Result<Vec<Value>, ApiFail> {
        let q = [
            ("filter[contentable_id][is]", contentable_id.to_string()),
            ("filter[contentable_type][is]", contentable_type.to_string()),
            ("sort", "index".to_string()),
            ("page[limit]", "0".to_string()),
        ];
        match self.get_json("/course/contents/", Some(&q))? {
            Value::Array(a) => Ok(a),
            Value::Object(mut m) => match m.remove("data") {
                Some(Value::Array(a)) => Ok(a),
                Some(v) => Ok(vec![v]),
                None => Ok(vec![]),
            },
            _ => Ok(vec![]),
        }
    }

    /// Verified live: module rows live under `App\Models\Modules`
    /// (course text rows under `App\Models\Courses`).
    pub fn iter_contents(&self, contentable_id: &str) -> (Option<String>, Vec<Value>) {
        for t in [
            "App\\Models\\Modules",
            "App\\Models\\Activities",
            "App\\Models\\Courses",
            "activities",
            "modules",
            "App/Models/Courses",
        ] {
            match self.list_contents(contentable_id, t) {
                Ok(items) if !items.is_empty() => return (Some(t.to_string()), items),
                _ => continue,
            }
        }
        (None, vec![])
    }

    /// Modules of a course. The detail response may be bare (no `meta`)
    /// or wrapped in `data` — both shapes are handled (see protocol §10).
    pub fn course_modules(&self, course_id: &str) -> Result<Vec<Module>, ApiFail> {
        let detail = self.get_course(course_id)?;
        let mut raws: Vec<Value> = vec![];
        if let Some(inc) = detail.get("included_raw").and_then(|x| x.as_object()) {
            for (k, m) in inc {
                if (k.starts_with("module") || k.contains("activit") || m.is_object())
                    && (m.get("name").is_some() || m.get("title").is_some())
                {
                    raws.push(m.clone());
                }
            }
        }
        if raws.is_empty() {
            let mut datas = vec![];
            if let Some(d) = detail.get("data") {
                datas.push(d);
            }
            datas.push(&detail);
            for data in datas {
                for key in ["modules", "activities", "items"] {
                    if let Some(arr) = data.get(key).and_then(|x| x.as_array()) {
                        raws.extend(arr.clone());
                    }
                }
                if !raws.is_empty() {
                    break;
                }
            }
        }
        let mut seen = std::collections::HashSet::new();
        let mut out = vec![];
        for m in raws {
            let mid = m
                .get("id")
                .map(|x| {
                    if let Some(s) = x.as_str() {
                        s.to_string()
                    } else {
                        x.to_string()
                    }
                })
                .unwrap_or_default();
            if mid.is_empty() || !seen.insert(mid) {
                continue;
            }
            out.push(Module::new(&m));
        }
        Ok(out)
    }

    /// Contents of a module, plus its activities' contents, deduped by id.
    pub fn module_items(&self, module: &Module) -> Result<Vec<Item>, ApiFail> {
        let (_, items) = self.iter_contents(&module.id);
        let mut out: Vec<Item> = items.into_iter().map(Item::new).collect();
        let mut seen: std::collections::HashSet<String> =
            out.iter().map(|i| i.id.clone()).collect();
        for a in &module.activities {
            let aid = a
                .get("id")
                .map(|x| {
                    if let Some(s) = x.as_str() {
                        s.to_string()
                    } else {
                        x.to_string()
                    }
                })
                .unwrap_or_default();
            if aid.is_empty() {
                continue;
            }
            let (_, sub) = self.iter_contents(&aid);
            for v in sub {
                let it = Item::new(v);
                if seen.insert(it.id.clone()) {
                    out.push(it);
                }
            }
        }
        Ok(out)
    }

    // ---------- file url ----------
    pub fn cdn_url(content: &str) -> Option<String> {
        let c = content.trim();
        if c.is_empty() {
            return None;
        }
        if c.starts_with("http://") || c.starts_with("https://") {
            return Some(c.to_string());
        }
        if c.starts_with('/') {
            return Some(format!("{CDN_BASE}{c}"));
        }
        if c.starts_with("storage/") || c.starts_with("files/") {
            return Some(format!("{STORAGE_DL}{}", url_encode(c)));
        }
        if !c.contains(' ') && !c.contains('\n') && c.contains('.') && c.len() < 500 {
            return Some(format!("{CDN_BASE}/{}", c.trim_start_matches('/')));
        }
        None
    }

    /// Resolve downloadable URL. Returns (url, filename, size) or None.
    pub fn file_url(item: &Value) -> Option<(String, String, Option<u64>)> {
        let ctype = item
            .get("contents_type")
            .or_else(|| item.get("type"))
            .and_then(|x| x.as_str())
            .unwrap_or("")
            .to_lowercase();
        if is_viewer_type(&ctype) {
            return None;
        }
        let raw = item
            .get("content")
            .or_else(|| item.get("url"))
            .or_else(|| item.get("file"))
            .and_then(|x| x.as_str())
            .unwrap_or("")
            .to_string();
        let mut url = Self::cdn_url(&raw);
        if url.is_none() {
            if let Some(fid) = item.get("file_id").or_else(|| item.get("file")) {
                let s = if let Some(x) = fid.as_str() {
                    x.to_string()
                } else {
                    fid.to_string()
                };
                if !s.is_empty() {
                    url = Some(format!("{STORAGE_DL}{}", url_encode(&s)));
                }
            }
        }
        let url = url?;
        let mut name = item
            .get("name")
            .or_else(|| item.get("filename"))
            .and_then(|x| x.as_str())
            .unwrap_or("file")
            .to_string();
        let id = item.get("id").map(|x| {
            if let Some(s) = x.as_str() {
                s.to_string()
            } else {
                x.to_string()
            }
        });
        if name.is_empty() {
            name = format!("file_{}", id.clone().unwrap_or_else(|| "x".to_string()));
        }
        if !name.rsplit('/').next().unwrap_or("").contains('.') {
            for ext in [".pdf", ".pptx", ".ppt", ".mp4", ".mp3", ".zip", ".docx"] {
                if url.to_lowercase().contains(ext) {
                    name.push_str(ext);
                    break;
                }
            }
        }
        let size = item
            .get("size")
            .and_then(|x| x.as_u64().or_else(|| x.as_str()?.parse().ok()));
        Some((url, name, size))
    }

    // ---------- download ----------
    /// Streaming download with resume + atomic rename.
    pub fn download(
        &self,
        url: &str,
        dest: &Path,
        expected_size: Option<u64>,
    ) -> Result<DlOutcome, ApiFail> {
        if let Ok(meta) = std::fs::metadata(dest) {
            if expected_size.map(|e| meta.len() == e).unwrap_or(false) {
                return Ok(DlOutcome::SkipSameSize);
            }
        } else if dest.extension().is_none() && sibling_same_size(dest, expected_size) {
            // an older run already saved this file WITH its detected extension
            return Ok(DlOutcome::SkipSameSize);
        }
        if let Some(p) = dest.parent() {
            std::fs::create_dir_all(p).map_err(|e| ApiFail::Net(e.to_string()))?;
        }
        let part: PathBuf = {
            let mut s = dest.as_os_str().to_owned();
            s.push(".part");
            PathBuf::from(s)
        };
        let resume_from = std::fs::metadata(&part).map(|m| m.len()).unwrap_or(0);
        let mut attempt_range = if resume_from > 0
            && expected_size.map(|e| resume_from < e).unwrap_or(true)
        {
            Some(resume_from)
        } else {
            if part.exists() && resume_from == 0 {
                let _ = std::fs::remove_file(&part);
            }
            None
        };
        let mut last_err = String::new();
        for attempt in 0..3 {
            let mut req = self.http.get(url);
            if needs_auth(url) {
                req = req.header("Authorization", self.sess.bearer());
            }
            if let Some(from) = attempt_range {
                req = req.header("Range", format!("bytes={from}-"));
            }
            let resp = match req.send() {
                Err(e) => {
                    last_err = e.to_string();
                    std::thread::sleep(POLITE_DELAY * (attempt + 1));
                    attempt_range = std::fs::metadata(&part).map(|m| m.len()).ok();
                    continue;
                }
                Ok(r) => r,
            };
            let st = resp.status();
            if st == StatusCode::UNAUTHORIZED || st == StatusCode::FORBIDDEN {
                return Err(ApiFail::AuthExpired(
                    "session expired during download, run `login` again".to_string(),
                ));
            }
            if st.as_u16() == 416 {
                let _ = std::fs::remove_file(&part);
                attempt_range = None;
                continue;
            }
            if st.is_server_error() {
                last_err = format!("server error {st}");
                std::thread::sleep(POLITE_DELAY * (attempt + 1));
                continue;
            }
            if !st.is_success() {
                return Err(ApiFail::Http(st.as_u16(), "file fetch failed".to_string()));
            }
            let total: Option<u64> = resp.headers().get("Content-Length").and_then(|v| {
                v.to_str().ok()?.parse::<u64>().ok()
            });
            let content_type: Option<String> = resp
                .headers()
                .get("Content-Type")
                .and_then(|v| v.to_str().ok())
                .map(|s| s.to_string());
            let total_n = total.map(|t| t + attempt_range.unwrap_or(0));
            let append = attempt_range.is_some();
            let mut file = std::fs::OpenOptions::new()
                .create(true)
                .write(true)
                .append(append)
                .truncate(!append)
                .open(&part)
                .map_err(|e| ApiFail::Net(e.to_string()))?;
            let mut done = attempt_range.unwrap_or(0);
            let t0 = std::time::Instant::now();
            let show = total_n.map(|t| t > 2 * 1024 * 1024).unwrap_or(false);
            let mut reader = resp;
            let mut buf = [0u8; 1024 * 1024];
            let mut failed: Option<String> = None;
            loop {
                match reader.read(&mut buf) {
                    Ok(0) => break,
                    Ok(n) => {
                        if file.write_all(&buf[..n]).is_err() {
                            failed = Some("write failed".to_string());
                            break;
                        }
                        done += n as u64;
                        if show {
                            let el = t0.elapsed().as_secs_f64().max(0.01);
                            let pct = total_n.map(|t| (done * 100 / t).min(100)).unwrap_or(0);
                            eprint!("\r  {pct}% {:.0} KB/s", done as f64 / el / 1024.0);
                        }
                    }
                    Err(e) => {
                        failed = Some(e.to_string());
                        break;
                    }
                }
            }
            if show {
                eprint!("\n");
            }
            if let Some(e) = failed {
                last_err = e;
                std::thread::sleep(POLITE_DELAY * (attempt + 1));
                attempt_range = std::fs::metadata(&part).map(|m| m.len()).ok();
                continue;
            }
            std::fs::rename(&part, dest).map_err(|e| ApiFail::Net(e.to_string()))?;
            let final_dest =
                ensure_extension(dest, content_type.as_deref()).map_err(ApiFail::Net)?;
            std::thread::sleep(POLITE_DELAY);
            return Ok(DlOutcome::Done(final_dest));
        }
        if last_err.is_empty() {
            last_err = "download failed".to_string();
        }
        Err(ApiFail::Net(last_err))
    }
}

/// What `Client::download` did (final path included: the extension
/// may have been detected from the file contents after saving).
pub enum DlOutcome {
    Done(PathBuf),
    SkipSameSize,
}

/// Guess a file extension from Content-Type and/or magic bytes.
/// Azure Blob URLs carry no filename, so Edunex files often arrive nameless.
fn sniff_ext(path: &Path, content_type: Option<&str>) -> Option<&'static str> {
    if let Some(ct) = content_type {
        match ct.split(';').next().unwrap_or("").trim().to_lowercase().as_str() {
            "application/pdf" => return Some("pdf"),
            "application/vnd.openxmlformats-officedocument.presentationml.presentation" => {
                return Some("pptx")
            }
            "application/vnd.ms-powerpoint" => return Some("ppt"),
            "application/vnd.openxmlformats-officedocument.wordprocessingml.document" => {
                return Some("docx")
            }
            "application/msword" => return Some("doc"),
            "application/vnd.openxmlformats-officedocument.spreadsheetml.sheet" => {
                return Some("xlsx")
            }
            "video/mp4" => return Some("mp4"),
            "audio/mpeg" => return Some("mp3"),
            _ => {}
        }
    }
    let mut f = std::fs::File::open(path).ok()?;
    let mut head = [0u8; 8];
    let n = f.read(&mut head).ok()?;
    if n >= 5 && &head[..5] == b"%PDF-" {
        return Some("pdf");
    }
    if n >= 4 && head[0] == b'P' && head[1] == b'K' && head[2] == 3 && head[3] == 4 {
        drop(f);
        return zip_kind(path);
    }
    if n >= 8 && head == [0xD0, 0xCF, 0x11, 0xE0, 0xA1, 0xB1, 0x1A, 0xE1] {
        return Some("ppt"); // old MS Office binary; on Edunex it's slides
    }
    if n >= 8 && head == [0x89, b'P', b'N', b'G', 0x0D, 0x0A, 0x1A, 0x0A] {
        return Some("png");
    }
    if n >= 3 && &head[..3] == b"GIF" {
        return Some("gif");
    }
    if n >= 3 && head[0] == 0xFF && head[1] == 0xD8 && head[2] == 0xFF {
        return Some("jpg");
    }
    None
}

/// pptx/docx/xlsx are all zips — peek inside to pick the right extension.
fn zip_kind(path: &Path) -> Option<&'static str> {
    let f = std::fs::File::open(path).ok()?;
    let z = zip::ZipArchive::new(f).ok()?;
    let mut kind = "zip";
    for name in z.file_names() {
        if name.starts_with("ppt/") {
            return Some("pptx");
        }
        if name.starts_with("word/") {
            kind = "docx";
        }
        if name.starts_with("xl/") {
            kind = "xlsx";
        }
    }
    Some(kind)
}

/// If `dest` has no extension, detect the real type and rename.
/// Returns the final path (unchanged when already fine or type unknown).
fn ensure_extension(dest: &Path, content_type: Option<&str>) -> Result<PathBuf, String> {
    if dest.extension().is_some() {
        return Ok(dest.to_path_buf());
    }
    let ext = match sniff_ext(dest, content_type) {
        Some(e) => e,
        None => return Ok(dest.to_path_buf()),
    };
    let mut cand = dest.with_extension(ext);
    if cand.exists() {
        let same_len = std::fs::metadata(&cand).map(|m| m.len()).ok()
            == std::fs::metadata(dest).map(|m| m.len()).ok();
        if same_len {
            std::fs::remove_file(&cand).map_err(|e| e.to_string())?;
        } else {
            let stem = dest
                .file_stem()
                .map(|s| s.to_string_lossy().to_string())
                .unwrap_or_default();
            let mut i = 1u32;
            loop {
                i += 1;
                let alt = dest.with_file_name(format!("{stem} ({i}).{ext}"));
                if !alt.exists() {
                    cand = alt;
                    break;
                }
                if i > 50 {
                    return Ok(dest.to_path_buf());
                }
            }
        }
    }
    std::fs::rename(dest, &cand).map_err(|e| e.to_string())?;
    Ok(cand)
}

/// An older run may have saved this file WITH its detected extension
/// (e.g. `Peluang.pptx` vs current `Peluang`). Same stem + same size = skip.
fn sibling_same_size(dest: &Path, expected_size: Option<u64>) -> bool {
    let e = match expected_size {
        Some(x) => x,
        None => return false,
    };
    let parent = match dest.parent() {
        Some(p) => p,
        None => return false,
    };
    let stem = match dest.file_stem().and_then(|s| s.to_str()) {
        Some(s) => s.to_lowercase(),
        None => return false,
    };
    let rd = match std::fs::read_dir(parent) {
        Ok(r) => r,
        Err(_) => return false,
    };
    for ent in rd.flatten() {
        let p = ent.path();
        if p == dest || p.extension().is_none() {
            continue;
        }
        let same = p
            .file_stem()
            .and_then(|s| s.to_str())
            .map(|s| s.to_lowercase() == stem)
            .unwrap_or(false);
        if same && std::fs::metadata(&p).map(|m| m.len() == e).unwrap_or(false) {
            return true;
        }
    }
    false
}

/// Identity for pagination dedupe: `id` when present, else the whole JSON
/// (some list rows carry no id — without this fallback every page looks
/// "fresh" and the pager runs to its 10-page cap = 200 phantom rows).
fn item_key(x: &Value) -> String {
    match x.get("id") {
        Some(id) => id.to_string(),
        None => serde_json::to_string(x).unwrap_or_default(),
    }
}

/// Only first-party hosts get the Bearer token. External file hosts
/// (e.g. Azure Blob signed URLs) reject unknown `Authorization` headers
/// with 400, so never send it there.
fn needs_auth(url: &str) -> bool {
    let host = url
        .split("://")
        .nth(1)
        .unwrap_or("")
        .split('/')
        .next()
        .unwrap_or("")
        .to_lowercase();
    host.ends_with("cognisia.id") || host.ends_with("itb.ac.id")
}

fn url_encode(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for b in s.bytes() {
        if b.is_ascii_alphanumeric() || matches!(b, b'-' | b'_' | b'.' | b'~' | b'/') {
            out.push(b as char);
        } else {
            out.push_str(&format!("%{b:02X}"));
        }
    }
    out
}

/// Flatten JSON:API {data, included, meta} into plain values.
pub fn deser(body: &Value) -> Value {
    let data = match body.get("data") {
        Some(d) => d,
        None => return body.clone(),
    };
    let included = body.get("included");
    let mut bykey: HashMap<String, &Value> = HashMap::new();
    if let Some(Value::Array(arr)) = included {
        for inc in arr {
            if let (Some(t), Some(id)) = (
                inc.get("type").and_then(|x| x.as_str()),
                inc.get("id").map(|x| {
                    if let Some(s) = x.as_str() {
                        s.to_string()
                    } else {
                        x.to_string()
                    }
                }),
            ) {
                bykey.insert(format!("{t}-{id}"), inc);
            }
        }
    }
    fn flat(item: &Value) -> Value {
        let mut m = serde_json::Map::new();
        if let Some(id) = item.get("id") {
            m.insert("id".to_string(), id.clone());
        }
        if let Some(t) = item.get("type") {
            m.insert("type".to_string(), t.clone());
        }
        if let Some(attrs) = item.get("attributes").and_then(|x| x.as_object()) {
            for (k, v) in attrs {
                m.insert(k.clone(), v.clone());
            }
        }
        Value::Object(m)
    }
    fn resolve(item: &Value, bykey: &HashMap<String, &Value>) -> Value {
        let mut f = flat(item);
        if let Some(rels) = item.get("relationships").and_then(|x| x.as_object()) {
            let obj = f.as_object_mut().unwrap();
            for (name, rel) in rels {
                let rd = rel.get("data");
                match rd {
                    Some(Value::Object(o)) => {
                        let key = format!(
                            "{}-{}",
                            o.get("type").and_then(|x| x.as_str()).unwrap_or(""),
                            o.get("id").map(|x| {
                                if let Some(s) = x.as_str() {
                                    s.to_string()
                                } else {
                                    x.to_string()
                                }
                            }).unwrap_or_default()
                        );
                        let v = bykey.get(&key).map(|r| flat(r)).unwrap_or_else(|| {
                            serde_json::json!({"id": o.get("id"), "type": o.get("type")})
                        });
                        obj.insert(name.clone(), v);
                    }
                    Some(Value::Array(arr)) => {
                        let mut out = vec![];
                        for e in arr {
                            let key = format!(
                                "{}-{}",
                                e.get("type").and_then(|x| x.as_str()).unwrap_or(""),
                                e.get("id").map(|x| {
                                    if let Some(s) = x.as_str() {
                                        s.to_string()
                                    } else {
                                        x.to_string()
                                    }
                                }).unwrap_or_default()
                            );
                            out.push(bykey.get(&key).map(|r| flat(r)).unwrap_or_else(|| {
                                serde_json::json!({"id": e.get("id"), "type": e.get("type")})
                            }));
                        }
                        obj.insert(name.clone(), Value::Array(out));
                    }
                    _ => {}
                }
            }
        }
        f
    }
    let data_v = match data {
        Value::Array(a) => Value::Array(a.iter().map(|x| resolve(x, &bykey)).collect()),
        single => resolve(single, &bykey),
    };
    if let Some(meta) = body.get("meta") {
        let mut inc_map = serde_json::Map::new();
        for (k, v) in &bykey {
            inc_map.insert(k.clone(), flat(v));
        }
        serde_json::json!({"data": data_v, "meta": meta, "included_raw": inc_map})
    } else {
        data_v
    }
}

pub fn fmt_size(n: Option<u64>) -> String {
    match n {
        None => "?".to_string(),
        Some(x) if x < 1024 => format!("{x} B"),
        Some(x) if x < 1024 * 1024 => format!("{} KB", x / 1024),
        Some(x) => format!("{:.1} MB", x as f64 / 1024.0 / 1024.0),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn file_url_direct_and_viewer() {
        let pdf = json!({"id":"1","name":"s.pdf","contents_type":"pdf","content":"https://cdn-edunex.itb.ac.id/a/b.pdf","size":123});
        let (u, f, s) = Client::file_url(&pdf).unwrap();
        assert_eq!(u, "https://cdn-edunex.itb.ac.id/a/b.pdf");
        assert_eq!(f, "s.pdf");
        assert_eq!(s, Some(123));
        let zoom = json!({"id":"3","name":"meet","contents_type":"zoom","content":"https://zoom.us/j/1"});
        assert!(Client::file_url(&zoom).is_none());
        let text = json!({"id":"4","name":"t","contents_type":"text","content":"<p>hi</p>"});
        assert!(Client::file_url(&text).is_none());
    }

    #[test]
    fn cdn_prefix() {
        assert_eq!(
            Client::cdn_url("/a/b.pptx").as_deref(),
            Some("https://cdn-edunex.itb.ac.id/a/b.pptx")
        );
        assert!(Client::cdn_url("").is_none());
    }

    #[test]
    fn deser_plain_passthrough() {
        let v = json!({"name":"x"});
        assert_eq!(deser(&v), v);
    }

    #[test]
    fn sniff_detects_type() {
        let dir = std::env::temp_dir().join("eduway-sniff-test");
        std::fs::create_dir_all(&dir).unwrap();
        let pdf = dir.join("a.bin");
        std::fs::write(&pdf, b"%PDF-1.7 fake").unwrap();
        assert_eq!(sniff_ext(&pdf, None), Some("pdf"));
        let pdf2 = dir.join("b.bin");
        std::fs::write(&pdf2, b"hello").unwrap();
        assert_eq!(sniff_ext(&pdf2, Some("application/pdf")), Some("pdf"));
        assert_eq!(sniff_ext(&pdf2, None), None);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn tab_qs_filter() {
        assert_eq!(Tab::Active.qs(), Some(("filter[is_active][is]", "1")));
        assert_eq!(Tab::All.qs(), None);
    }
}
