//! Semi-automatic login (`login --browser`).
//!
//! Full redirect-back OAuth isn't possible: Edunex SSO runs on Microsoft's
//! tenant with ITB/Cognisia's own app registration, and no third-party app
//! can read another site's localStorage (same-origin policy).
//! So instead: open the real login page, the user logs in normally, then a
//! one-click bookmarklet POSTs the `auth` value back to a localhost
//! listener here. No copy-paste, still exactly one user action.

use crate::api::Client;
use crate::session::{self, Session};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

pub const TIMEOUT: Duration = Duration::from_secs(5 * 60);
const MAX_BODY: usize = 64 * 1024;

pub enum Event {
    Ready { bookmarklet: String, page: String },
    Done(Result<Session, String>),
}

/// One-click bookmarklet, to be saved as a bookmark URL and clicked
/// while on the logged-in Edunex tab. 127.0.0.1 is a secure context,
/// so the https page is allowed to POST here (with CORS, handled below).
pub fn bookmarklet(port: u16) -> String {
    format!(
        "javascript:(function(){{var a=localStorage.getItem('auth');if(!a){{alert('belum login di tab ini - login dulu di edunex');return;}}fetch('http://127.0.0.1:{port}/capture',{{method:'POST',headers:{{'Content-Type':'application/json'}},body:a}}).then(function(r){{return r.json();}}).then(function(j){{alert(j.ok?'ok! balik ke eduway':'gagal: '+(j.error||'unknown'));}}).catch(function(e){{alert('gagal: '+e);}});}})();"
    )
}

fn header(k: &'static str, v: &'static str) -> tiny_http::Header {
    tiny_http::Header::from_bytes(k.as_bytes(), v.as_bytes()).unwrap()
}

fn cors<R: std::io::Read>(r: tiny_http::Response<R>) -> tiny_http::Response<R> {
    r.with_header(header("Access-Control-Allow-Origin", "*"))
        .with_header(header("Access-Control-Allow-Methods", "GET, POST, OPTIONS"))
        .with_header(header("Access-Control-Allow-Headers", "Content-Type"))
}

fn page_html() -> String {
    "<!doctype html><html><body style=\"font-family:sans-serif;max-width:640px;margin:40px auto\">\
        <h2>eduway login</h2>\
        <p id=\"st\">menunggu token dari browser...</p>\
        <ol><li>login di tab Edunex sampai dashboard kebuka</li>\
        <li>klik bookmarklet eduway di tab itu</li>\
        <li>balik ke app</li></ol>\
        <script>setInterval(function(){fetch('/status').then(function(r){return r.json();}).then(function(j){if(j.got){document.getElementById('st').textContent='diterima! balik ke app.';}});},2000);</script>\
        </body></html>"
        .to_string()
}

fn handle(mut req: tiny_http::Request, got: &Arc<Mutex<Option<serde_json::Value>>>) {
    let method = req.method().clone();
    let url = req.url().to_string();
    let is_options = matches!(method, tiny_http::Method::Options);
    if is_options {
        let _ = req.respond(cors(tiny_http::Response::from_string("")));
        return;
    }
    if url == "/" {
        let _ = req.respond(cors(tiny_http::Response::from_string(page_html())));
        return;
    }
    if url == "/status" {
        let g = got.lock().map(|o| o.is_some()).unwrap_or(false);
        let body = if g {
            "{\"got\":true}"
        } else {
            "{\"got\":false}"
        };
        let _ = req.respond(cors(
            tiny_http::Response::from_string(body)
                .with_header(header("Content-Type", "application/json")),
        ));
        return;
    }
    if url == "/capture" && matches!(method, tiny_http::Method::Post) {
        // capped read: localhost only, but don't trust Content-Length blindly
        let mut body: Vec<u8> = Vec::new();
        let mut chunk = [0u8; 8192];
        let mut ok = true;
        loop {
            if body.len() > MAX_BODY {
                ok = false;
                break;
            }
            match req.as_reader().read(&mut chunk) {
                Ok(0) => break,
                Ok(n) => body.extend_from_slice(&chunk[..n]),
                Err(_) => {
                    ok = false;
                    break;
                }
            }
        }
        let body = if ok { String::from_utf8(body).unwrap_or_default() } else { String::new() };
        let reply = if !ok || body.len() > MAX_BODY {
            "{\"ok\":false,\"error\":\"body kebesaran / gagal baca\"}"
        } else {
            match serde_json::from_str::<serde_json::Value>(&body) {
                Err(_) => "{\"ok\":false,\"error\":\"bukan JSON\"}",
                Ok(v) => {
                    *got.lock().unwrap() = Some(v);
                    "{\"ok\":true}"
                }
            }
        };
        let _ = req.respond(cors(
            tiny_http::Response::from_string(reply)
                .with_header(header("Content-Type", "application/json")),
        ));
        return;
    }
    let _ = req.respond(cors(tiny_http::Response::from_string("nope")));
}

/// Run the flow. `open_browser` opens the Edunex login page (off in tests).
/// Blocks until a token arrives + verifies, or the timeout hits.
pub fn run(timeout: Duration, open_browser: bool, mut emit: impl FnMut(Event)) {
    let server = match tiny_http::Server::http("127.0.0.1:0") {
        Ok(s) => s,
        Err(e) => {
            emit(Event::Done(Err(format!("ga bisa buka port lokal: {e}"))));
            return;
        }
    };
    let port = server
        .server_addr()
        .to_ip()
        .map(|a| a.port())
        .unwrap_or(0);
    let page = format!("http://127.0.0.1:{port}/");
    emit(Event::Ready {
        bookmarklet: bookmarklet(port),
        page: page.clone(),
    });
    if open_browser {
        if open::that("https://edunex.itb.ac.id/pages/login").is_err() {
            emit(Event::Done(Err(
                "ga bisa buka browser otomatis — buka manual: https://edunex.itb.ac.id/pages/login".to_string(),
            )));
            return;
        }
    }
    let got: Arc<Mutex<Option<serde_json::Value>>> = Arc::new(Mutex::new(None));
    let deadline = Instant::now() + timeout;
    while Instant::now() < deadline {
        if got.lock().map(|o| o.is_some()).unwrap_or(false) {
            break;
        }
        let step = deadline
            .checked_duration_since(Instant::now())
            .unwrap_or(Duration::ZERO)
            .min(Duration::from_secs(1));
        match server.recv_timeout(step) {
            Ok(Some(req)) => handle(req, &got),
            Ok(None) => {}
            Err(_) => break,
        }
    }
    let raw = got.lock().unwrap().clone();
    match raw {
        None => emit(Event::Done(Err(
            "waktu habis — login di browser dulu baru klik bookmarklet, atau ulangi".to_string(),
        ))),
        Some(v) => match session::normalize(&v) {
            None => emit(Event::Done(Err(
                "data dari browser bukan session valid".to_string(),
            ))),
            Some(sess) => {
                let bad = Client::new(sess.clone())
                    .map_err(|e| format!("{e:?}"))
                    .and_then(|c| c.me().map(|_| ()).map_err(|e| format!("{e}")));
                match bad {
                    Ok(_) => emit(Event::Done(Ok(sess))),
                    Err(e) => emit(Event::Done(Err(format!("token ditolak server: {e}")))),
                }
            }
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bookmarklet_targets_localhost() {
        let b = bookmarklet(17849);
        assert!(b.starts_with("javascript:"));
        assert!(b.contains("http://127.0.0.1:17849/capture"));
        assert!(b.contains("localStorage.getItem('auth')"));
    }

    #[test]
    fn capture_plumbing() {
        // full loop with a fake token: capture must work, verify must fail cleanly
        let (tx, rx) = std::sync::mpsc::channel();
        let h = std::thread::spawn(move || {
            run(Duration::from_secs(20), false, |ev| {
                let _ = tx.send(match ev {
                    Event::Ready { bookmarklet, page } => (bookmarklet, page, true),
                    Event::Done(_) => (String::new(), String::new(), false),
                });
            });
        });
        let (bm, page, ready) = rx.recv_timeout(Duration::from_secs(10)).unwrap();
        assert!(ready && bm.contains("/capture"));
        let port: u16 = page
            .trim_start_matches("http://127.0.0.1:")
            .trim_end_matches('/')
            .parse()
            .unwrap();
        let body = r#"{"accessToken":"bogus","refreshToken":"x"}"#;
        let http = reqwest::blocking::Client::new();
        let pre = http
            .request(
                reqwest::Method::OPTIONS,
                format!("http://127.0.0.1:{port}/capture"),
            )
            .header("Origin", "https://edunex.itb.ac.id")
            .header("Access-Control-Request-Method", "POST")
            .timeout(Duration::from_secs(10))
            .send()
            .unwrap();
        assert_eq!(
            pre.headers()
                .get("access-control-allow-origin")
                .map(|v| v.to_str().unwrap()),
            Some("*")
        );
        let resp = http
            .post(format!("http://127.0.0.1:{port}/capture"))
            .header("Content-Type", "application/json")
            .body(body.to_string())
            .timeout(Duration::from_secs(10))
            .send()
            .unwrap();
        assert!(resp.text().unwrap().contains("\"ok\":true"));
        match rx.recv_timeout(Duration::from_secs(30)).unwrap() {
            (_, _, false) => {} // Done arrived (verify failed on bogus token, as expected)
            _ => panic!("expected Done"),
        }
        h.join().unwrap();
    }
}
