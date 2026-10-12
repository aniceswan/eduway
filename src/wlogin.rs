//! Fully automatic login via a real browser window (`login --webview`).
//!
//! Why not an embedded webview: wry/webkit paints blank on several
//! Linux/XWayland setups (proven with screenshots), needs 100MB+ of dev
//! libraries, and occasionally trips SSO bot checks. Driving the real
//! Chrome over CDP gives the same UX (window opens, user logs in, the
//! rest is automatic) with zero render risk and no extra system deps.
//!
//! Flow: launch Chrome with a throwaway profile + remote debugging,
//! open the Edunex login page, poll `localStorage.getItem('auth')`
//! until it appears, verify against `/login/me`, save, kill Chrome.

use crate::api::Client;
use crate::session;
use std::io::{BufRead, BufReader};
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};

const LOGIN_URL: &str = "https://edunex.itb.ac.id/pages/login";
const TIMEOUT: Duration = Duration::from_secs(10 * 60);
const POLL: Duration = Duration::from_millis(1500);

const CHROME_CANDIDATES: &[&str] = &[
    "google-chrome",
    "google-chrome-stable",
    "chromium",
    "chromium-browser",
    "microsoft-edge",
];

fn find_chrome() -> Option<String> {
    // PATH lookup first, then well-known absolute paths.
    for bin in CHROME_CANDIDATES {
        if Command::new(bin)
            .arg("--version")
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status()
            .map(|s| s.success())
            .unwrap_or(false)
        {
            return Some(bin.to_string());
        }
    }
    for p in [
        "/usr/bin/google-chrome",
        "/usr/bin/chromium",
        "/usr/bin/chromium-browser",
        "/opt/google/chrome/chrome",
        r"C:\Program Files\Google\Chrome\Application\chrome.exe",
        r"C:\Program Files (x86)\Google\Chrome\Application\chrome.exe",
    ] {
        if std::path::Path::new(p).exists() {
            return Some(p.to_string());
        }
    }
    None
}

/// Profile dir for the throwaway login browser (deleted afterwards).
fn profile_dir() -> std::path::PathBuf {
    std::env::temp_dir().join(format!("eduway-chrome-{}", std::process::id()))
}

/// Launch headful Chrome with remote debugging; returns (child, cdp port).
fn launch(url: &str) -> Result<(Child, u16, std::path::PathBuf), String> {
    let bin = find_chrome().ok_or_else(|| {
        "Chrome/Chromium tidak ketemu. Install dulu, atau pakai `login --browser`".to_string()
    })?;
    let profile = profile_dir();
    let _ = std::fs::create_dir_all(&profile);
    let mut child = Command::new(&bin)
        .arg(format!("--user-data-dir={}", profile.display()))
        .arg("--remote-debugging-port=0")
        .arg("--no-first-run")
        .arg("--no-default-browser-check")
        // headed Chrome must use X11 here (Wayland ozone is flaky headful);
        // XWayland is always present on GNOME/KDE Wayland sessions.
        .env("XDG_SESSION_TYPE", "x11")
        .env("WAYLAND_DISPLAY", "")
        .arg(url)
        .stderr(Stdio::piped())
        .stdout(Stdio::null())
        .spawn()
        .map_err(|e| format!("gagal jalanin {bin}: {e}"))?;
    // DevTools prints: "DevTools listening on ws://127.0.0.1:PORT/..."
    let stderr = child.stderr.take().ok_or("no stderr")?;
    let reader = BufReader::new(stderr);
    for line in reader.lines().take(200) {
        let line = line.unwrap_or_default();
        if let Some(i) = line.find("ws://127.0.0.1:") {
            let rest = &line[i + "ws://127.0.0.1:".len()..];
            let port: String = rest.chars().take_while(|c| c.is_ascii_digit()).collect();
            if let Ok(p) = port.parse::<u16>() {
                return Ok((child, p, profile));
            }
        }
        if line.contains("ERROR") && line.contains("devtools") {
            break;
        }
    }
    let _ = child.kill();
    Err("ga dapat port debugging dari Chrome".to_string())
}

fn page_ws(port: u16) -> Result<String, String> {
    let list: serde_json::Value = reqwest::blocking::Client::new()
        .get(format!("http://127.0.0.1:{port}/json/list"))
        .timeout(Duration::from_secs(10))
        .send()
        .map_err(|e| format!("CDP list gagal: {e}"))?
        .json()
        .map_err(|e| format!("CDP list bukan JSON: {e}"))?;
    list.as_array()
        .and_then(|a| {
            a.iter().find(|t| {
                t.get("type").and_then(|x| x.as_str()) == Some("page")
                    && t.get("url").and_then(|x| x.as_str()).map(|u| !u.starts_with("devtools://")).unwrap_or(false)
            })
        })
        .and_then(|t| t.get("webSocketDebuggerUrl").and_then(|x| x.as_str()))
        .map(|s| s.to_string())
        .ok_or_else(|| "tab Chrome tidak ketemu".to_string())
}

/// Blocking CDP `Runtime.evaluate`. Returns the raw JS value (or Null).
fn evaluate(ws_url: &str, js: &str, id: u64) -> Result<serde_json::Value, String> {
    use tungstenite::Message;
    let (mut sock, _) =
        tungstenite::connect(ws_url).map_err(|e| format!("CDP connect gagal: {e}"))?;
    let req = serde_json::json!({
        "id": id,
        "method": "Runtime.evaluate",
        "params": {"expression": js, "returnByValue": true},
    });
    sock.send(Message::Text(req.to_string().into()))
        .map_err(|e| format!("CDP send gagal: {e}"))?;
    let deadline = Instant::now() + Duration::from_secs(15);
    loop {
        if Instant::now() > deadline {
            return Err("CDP timeout".to_string());
        }
        let msg = sock
            .read()
            .map_err(|e| format!("CDP read gagal: {e}"))?;
        if let Message::Text(t) = msg {
            let v: serde_json::Value =
                serde_json::from_str(&t).map_err(|e| format!("CDP bukan JSON: {e}"))?;
            if v.get("id").and_then(|x| x.as_u64()) == Some(id) {
                return Ok(v
                    .pointer("/result/result/value")
                    .cloned()
                    .unwrap_or(serde_json::Value::Null));
            }
        }
    }
}

/// Wait until the page's `auth` localStorage appears. Pure CDP, no verify.
pub fn wait_for_auth(port: u16, timeout: Duration) -> Result<serde_json::Value, String> {
    let ws = page_ws(port)?;
    let start = Instant::now();
    let mut id = 1u64;
    while start.elapsed() < timeout {
        match evaluate(&ws, "localStorage.getItem('auth')", id) {
            Ok(v) if v.is_string() => return Ok(v),
            _ => {}
        }
        id += 1;
        std::thread::sleep(POLL);
    }
    Err("waktu habis menunggu login".to_string())
}

/// Opens the login window. Diverges: exit(0) on success (session saved),
/// exit(2) on cancel/error, exit(3) on timeout.
/// `EDUWAY_LOGIN_URL` overrides the start page (testing only).
pub fn run() -> ! {
    let start_url =
        std::env::var("EDUWAY_LOGIN_URL").unwrap_or_else(|_| LOGIN_URL.to_string());
    println!("membuka jendela login Chrome — login SSO seperti biasa, sisanya otomatis.");
    println!("(profil sementara, bukan Chrome harianmu; ditutup + dihapus otomatis)");
    let (mut child, port, profile) = match launch(&start_url) {
        Ok(x) => x,
        Err(e) => {
            eprintln!("FAIL {e}");
            std::process::exit(2);
        }
    };
    let raw = wait_for_auth(port, TIMEOUT);
    let _ = child.kill();
    let _ = std::fs::remove_dir_all(&profile);
    match raw {
        Err(e) => {
            if e.contains("waktu habis") {
                eprintln!("waktu habis (10 menit) — ulangi `login --webview`");
                std::process::exit(3);
            }
            eprintln!("FAIL {e}");
            std::process::exit(2);
        }
        Ok(v) => {
            let s = v.as_str().unwrap_or("");
            match serde_json::from_str::<serde_json::Value>(s)
                .ok()
                .and_then(|v| session::normalize(&v))
            {
                None => {
                    eprintln!("FAIL data dari browser bukan session valid");
                    std::process::exit(2);
                }
                Some(sess) => {
                    let ok = Client::new(sess.clone())
                        .ok()
                        .and_then(|c| c.me().ok())
                        .is_some();
                    if ok && session::save(&sess).is_ok() {
                        println!("ok, halo (session tersimpan otomatis)");
                        std::process::exit(0);
                    }
                    eprintln!("FAIL token ditolak server");
                    std::process::exit(2);
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn candidates_nonempty() {
        assert!(!CHROME_CANDIDATES.is_empty());
    }

    /// CDP against real headless Chrome: seed localStorage, expect capture.
    /// Needs a chrome binary; silently passes when none is installed (CI).
    #[test]
    fn cdp_poll_captures() {
        if find_chrome().is_none() {
            return;
        }
        let profile: std::path::PathBuf =
            std::env::temp_dir().join(format!("eduway-test-{}", std::process::id()));
        let _ = std::fs::create_dir_all(&profile);
        let mut child = Command::new(find_chrome().unwrap())
            .arg(format!("--user-data-dir={}", profile.display()))
            .arg("--remote-debugging-port=0")
            .arg("--headless=new")
            .arg("--no-first-run")
            .arg("about:blank")
            .stderr(Stdio::piped())
            .stdout(Stdio::null())
            .spawn()
            .expect("spawn chrome");
        let port = (|| {
            use std::io::BufRead;
            let stderr = child.stderr.take().unwrap();
            for line in std::io::BufReader::new(stderr).lines().take(100) {
                let line = line.unwrap_or_default();
                if let Some(i) = line.find("ws://127.0.0.1:") {
                    let p: String = line[i + "ws://127.0.0.1:".len()..]
                        .chars()
                        .take_while(|c| c.is_ascii_digit())
                        .collect();
                    if let Ok(p) = p.parse::<u16>() {
                        return Some(p);
                    }
                }
            }
            None
        })();
        let port = match port {
            Some(p) => p,
            None => {
                let _ = child.kill();
                panic!("no CDP port");
            }
        };
        // open a data: page and seed auth via evaluate itself
        let ws = page_ws(port).expect("page");
        let nav = serde_json::json!({"id": 100, "method": "Page.navigate",
            "params": {"url": "https://example.com/"}});
        {
            use tungstenite::Message;
            let (mut sock, _) = tungstenite::connect(&ws).expect("ws");
            sock.send(Message::Text(nav.to_string().into())).unwrap();
        }
        std::thread::sleep(Duration::from_secs(4));
        let seed = r#"localStorage.setItem('auth', '{"accessToken":"x"}'); "seeded""#;
        let v = evaluate(&ws, seed, 2).expect("seed");
        assert_eq!(v.as_str(), Some("seeded"));
        let got = wait_for_auth(port, Duration::from_secs(15)).expect("capture");
        assert_eq!(got.as_str(), Some("{\"accessToken\":\"x\"}"));
        let _ = child.kill();
        let _ = std::fs::remove_dir_all(&profile);
    }
}
