use anyhow::{Context, Result};
use serde_json::Value;
use std::fs;
use std::path::PathBuf;

#[cfg(unix)]
use std::os::unix::fs::PermissionsExt;

use crate::api::Tab;

pub const COURSES_TTL_SECS: u64 = 3600;

pub fn dir() -> PathBuf {
    dirs::home_dir()
        .unwrap_or_else(|| PathBuf::from("."))
        .join(".eduway")
}

pub fn session_file() -> PathBuf {
    dir().join("session.json")
}

pub fn courses_file(tab: Tab) -> PathBuf {
    dir().join(format!("courses_{}.json", tab.dir()))
}

#[derive(Debug, Clone, Default)]
pub struct Session {
    pub access_token: String,
    pub refresh_token: Option<String>,
    pub expiration: Option<String>,
    pub user_agent: String,
}

impl Session {
    pub fn bearer(&self) -> String {
        format!("Bearer {}", self.access_token)
    }
}

pub fn load() -> Option<Session> {
    let text = fs::read_to_string(session_file()).ok()?;
    let v: Value = serde_json::from_str(&text).ok()?;
    normalize(&v)
}

pub fn save(sess: &Session) -> Result<()> {
    let d = dir();
    fs::create_dir_all(&d).context("create ~/.eduway")?;
    let v = serde_json::json!({
        "access_token": sess.access_token,
        "refresh_token": sess.refresh_token,
        "expiration_date": sess.expiration,
        "user_agent": sess.user_agent,
    });
    let tmp = d.join("session.json.tmp");
    fs::write(&tmp, serde_json::to_string(&v).unwrap()).context("write session")?;
    #[cfg(unix)]
    fs::set_permissions(&tmp, fs::Permissions::from_mode(0o600)).ok();
    fs::rename(&tmp, session_file()).context("save session")?;
    Ok(())
}

/// Accept raw browser `auth` value, doc-format, or snake_case.
pub fn normalize(v: &Value) -> Option<Session> {
    let obj = v.as_object()?;
    let get = |keys: &[&str]| -> Option<String> {
        keys.iter()
            .filter_map(|k| obj.get(*k))
            .filter_map(|x| x.as_str())
            .next()
            .map(|s| s.to_string())
    };
    let mut at = get(&["access_token", "accessToken", "token"])?;
    at = at.trim().to_string();
    if at.to_lowercase().starts_with("bearer ") {
        at = at[7..].trim().to_string();
    }
    if at.is_empty() {
        return None;
    }
    Some(Session {
        access_token: at,
        refresh_token: get(&["refresh_token", "refreshToken"]),
        expiration: get(&["expiration_date", "expirationDate", "expires_in"])
            .or_else(|| obj.get("expires_in").map(|x| x.to_string())),
        user_agent: get(&["user_agent", "userAgent"])
            .unwrap_or_else(|| super::api::BROWSER_UA.to_string()),
    })
}

pub fn load_courses(tab: Tab) -> Option<Vec<Value>> {
    let p = courses_file(tab);
    let meta = fs::metadata(&p).ok()?;
    let age = std::time::SystemTime::now()
        .duration_since(meta.modified().ok()?)
        .ok()?
        .as_secs();
    if age > COURSES_TTL_SECS {
        return None;
    }
    let text = fs::read_to_string(&p).ok()?;
    let v: Vec<Value> = serde_json::from_str(&text).ok()?;
    Some(v)
}

pub fn save_courses(tab: Tab, courses: &[Value]) {
    let d = dir();
    if fs::create_dir_all(&d).is_err() {
        return;
    }
    if let Ok(s) = serde_json::to_string(courses) {
        let _ = fs::write(courses_file(tab), s);
    }
}

pub fn delete_courses(tab: Tab) {
    let _ = std::fs::remove_file(courses_file(tab));
}

pub fn describe(sess: &Session) -> String {
    let age = fs::metadata(session_file())
        .and_then(|m| m.modified())
        .ok()
        .and_then(|t| std::time::SystemTime::now().duration_since(t).ok())
        .map(|d| d.as_secs())
        .unwrap_or(0);
    format!(
        "saved {}s ago{}",
        age,
        if sess.refresh_token.is_some() {
            ", refresh available"
        } else {
            ""
        }
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn normalize_browser_raw() {
        let v: Value =
            serde_json::from_str(r#"{"accessToken":"AAA","refreshToken":"BBB","expirationDate":"x"}"#)
                .unwrap();
        let s = normalize(&v).unwrap();
        assert_eq!(s.access_token, "AAA");
        assert_eq!(s.refresh_token.as_deref(), Some("BBB"));
    }

    #[test]
    fn normalize_snake_and_bearer() {
        let v: Value = serde_json::from_str(r#"{"access_token":"Bearer AAA"}"#).unwrap();
        assert_eq!(normalize(&v).unwrap().access_token, "AAA");
        let v: Value = serde_json::from_str(r#"{"token":"AAA"}"#).unwrap();
        assert_eq!(normalize(&v).unwrap().access_token, "AAA");
        let v: Value = serde_json::from_str(r#"{"foo":1}"#).unwrap();
        assert!(normalize(&v).is_none());
    }
}
