use serde_json::Value;
use std::collections::BTreeMap;

use crate::api::Tab;

pub const VIEWER_HINT: &str = "[viewer]";
pub const CLOSED_HINT: &str = "[closed]";

pub fn is_viewer_type(ctype: &str) -> bool {
    matches!(
        ctype,
        "text"
            | "html"
            | "url"
            | "google-meet"
            | "zoom"
            | "ms-teams"
            | "cloudx"
            | "webex"
            | "scorm"
            | "conference"
    )
}

/// Sanitize a name for local FS: replace / \ : * ? " < > |, collapse repeats, cap length.
pub fn slug(name: &str, maxlen: usize) -> String {
    let mut s = name.trim().replace('/', "_");
    for ch in ['\\', ':', '*', '?', '"', '<', '>', '|'] {
        s = s.replace(ch, "_");
    }
    let s: String = s.split_whitespace().collect::<Vec<_>>().join(" ");
    let mut out = String::with_capacity(s.len());
    let mut prev_us = false;
    for ch in s.chars() {
        if ch == '_' {
            if !prev_us {
                out.push(ch);
            }
            prev_us = true;
        } else {
            out.push(ch);
            prev_us = false;
        }
    }
    let mut out = out.trim().to_string();
    if out.is_empty() {
        out = "untitled".to_string();
    }
    if out.len() > maxlen {
        out.truncate(maxlen);
        out = out.trim_end().to_string();
    }
    out
}

fn str_field(v: &Value, keys: &[&str]) -> String {
    for k in keys {
        if let Some(s) = v.get(*k).and_then(|x| x.as_str()) {
            return s.to_string();
        }
    }
    String::new()
}

#[derive(Debug, Clone)]
pub struct Item {
    pub raw: Value,
    pub id: String,
    pub name: String,
    pub ctype: String,
    pub size: Option<u64>,
    pub content: String,
}

impl Item {
    pub fn new(raw: Value) -> Self {
        let id = raw
            .get("id")
            .map(|x| {
                if let Some(s) = x.as_str() {
                    s.to_string()
                } else {
                    x.to_string()
                }
            })
            .unwrap_or_else(|| "?".to_string());
        let name = str_field(&raw, &["name", "filename", "title"]);
        let name = if name.is_empty() {
            format!("item-{id}")
        } else {
            name
        };
        let ctype = str_field(&raw, &["contents_type", "type"]).to_lowercase();
        let size = raw
            .get("size")
            .and_then(|x| x.as_u64().or_else(|| x.as_str()?.parse().ok()));
        let content = str_field(&raw, &["content", "url", "file"]);
        Self {
            raw,
            id,
            name,
            ctype,
            size,
            content,
        }
    }

    pub fn is_viewer_hint(&self) -> bool {
        if is_viewer_type(&self.ctype) {
            return true;
        }
        let c = self.content.trim();
        if c.is_empty() || c.starts_with('<') || c.len() < 4 {
            return true;
        }
        false
    }

    pub fn label(&self) -> String {
        if self.is_viewer_hint() {
            format!("{} {}", self.name, VIEWER_HINT)
        } else {
            self.name.clone()
        }
    }
}

#[derive(Debug, Clone)]
pub struct Module {
    pub id: String,
    pub name: String,
    pub is_open: bool,
    pub activities: Vec<Value>,
    pub items: Vec<Item>,
    /// set after the first contents fetch (even when empty).
    pub items_fetched: bool,
}

impl Module {
    pub fn new(raw: &Value) -> Self {
        let id = raw
            .get("id")
            .map(|x| {
                if let Some(s) = x.as_str() {
                    s.to_string()
                } else {
                    x.to_string()
                }
            })
            .unwrap_or_else(|| "?".to_string());
        let name = str_field(raw, &["name", "title"]);
        let name = if name.is_empty() {
            format!("module-{id}")
        } else {
            name
        };
        let is_open = match raw.get("is_open") {
            Some(Value::Bool(b)) => *b,
            Some(Value::Number(n)) => n.as_u64().unwrap_or(1) == 1,
            Some(Value::String(s)) => s == "1",
            _ => true,
        };
        let acts = raw
            .get("activities")
            .or_else(|| raw.get("activity"))
            .and_then(|x| x.as_array())
            .cloned()
            .unwrap_or_default();
        Self {
            id,
            name,
            is_open,
            activities: acts,
            items: vec![],
            items_fetched: false,
        }
    }

    pub fn label(&self) -> String {
        if self.is_open {
            format!("{}/", self.name)
        } else {
            format!("{}/ {}", self.name, CLOSED_HINT)
        }
    }
}

#[derive(Debug, Clone, Default)]
pub struct Course {
    pub id: String,
    pub name: String,
    pub raw: Value,
    pub modules: Vec<Module>,
    /// set after the first detail fetch (even when empty) so `ls`/`dl`
    /// don't refetch on every command; `refresh` resets it.
    pub detail_fetched: bool,
}

impl Course {
    pub fn new(raw: Value) -> Self {
        let id = raw
            .get("id")
            .map(|x| {
                if let Some(s) = x.as_str() {
                    s.to_string()
                } else {
                    x.to_string()
                }
            })
            .unwrap_or_else(|| "?".to_string());
        let title = str_field(&raw, &["name", "title"]);
        let title = if title.is_empty() {
            format!("course-{id}")
        } else {
            title
        };
        let code = str_field(&raw, &["code", "shortname"]);
        let name = if code.is_empty() {
            slug(&title, 80)
        } else {
            format!("{}-{}", code, slug(&title, 60))
        };
        let name = name.trim_matches('-').to_string();
        Self {
            id,
            name,
            raw,
            modules: vec![],
            detail_fetched: false,
        }
    }
}

#[derive(Debug, Default)]
pub struct Vfs {
    pub courses: BTreeMap<String, Course>,
    pub pos: Vec<String>,
}

impl Vfs {
    pub fn pwd(&self) -> String {
        if self.pos.is_empty() {
            "/".to_string()
        } else {
            format!("/{}", self.pos.join("/"))
        }
    }

    fn module_dir_names(&self, course: &Course) -> Vec<String> {
        course
            .modules
            .iter()
            .map(|m| format!("{}/", slug(&m.name, 120)))
            .collect()
    }

    fn item_names(&self, module: &Module) -> Vec<String> {
        let mut out = vec![];
        for a in &module.activities {
            let nm = a
                .get("name")
                .and_then(|x| x.as_str())
                .unwrap_or("activity");
            out.push(format!("{}/", slug(nm, 120)));
        }
        for it in &module.items {
            out.push(it.label());
        }
        out
    }

    pub fn ls(&self) -> Vec<String> {
        match self.pos.len() {
            // root = My Courses tabs, like the sidebar
            0 => [
                Tab::Active.dir().to_string(),
                Tab::Inactive.dir().to_string(),
                Tab::All.dir().to_string(),
            ]
            .iter()
            .map(|t| format!("{t}/"))
            .collect(),
            // tab = courses of that tab
            1 => self.courses.keys().map(|k| format!("{k}/")).collect(),
            // course = its modules
            2 => match self.courses.get(&self.pos[1]) {
                Some(c) => c.modules.iter().map(|m| m.label()).collect(),
                None => vec![],
            },
            // module = its files
            _ => {
                let c = match self.courses.get(&self.pos[1]) {
                    Some(c) => c,
                    None => return vec![],
                };
                for m in &c.modules {
                    if slug(&m.name, 120) == self.pos[2] {
                        return self.item_names(m);
                    }
                }
                vec![]
            }
        }
    }

    fn candidates(&self, cur: &[String]) -> Vec<String> {
        match cur.len() {
            0 => [
                Tab::Active.dir().to_string(),
                Tab::Inactive.dir().to_string(),
                Tab::All.dir().to_string(),
            ]
            .iter()
            .map(|t| format!("{t}/"))
            .collect(),
            1 => self.courses.keys().cloned().collect(),
            2 => match self.courses.get(&cur[1]) {
                Some(c) => self.module_dir_names(c),
                None => vec![],
            },
            _ => match self.courses.get(&cur[1]) {
                Some(c) => {
                    for m in &c.modules {
                        if slug(&m.name, 120) == cur[2] {
                            return self.item_names(m);
                        }
                    }
                    vec![]
                }
                None => vec![],
            },
        }
    }

    fn plain(cand: &str) -> String {
        let no_slash = cand.strip_suffix('/').unwrap_or(cand);
        match no_slash.split_once(" [") {
            Some((head, _)) => head.to_string(),
            None => no_slash.to_string(),
        }
    }

    /// Resolve `path` against current pos. Err lists similar names on typo.
    pub fn resolve(&self, path: &str) -> Result<Vec<String>, String> {
        if path.is_empty() || path == "." {
            return Ok(self.pos.clone());
        }
        if path == "/" || path == "~" {
            return Ok(vec![]);
        }
        let mut cur: Vec<String> = if path.starts_with('/') {
            vec![]
        } else {
            self.pos.clone()
        };
        for p in path.split('/') {
            if p.is_empty() || p == "." {
                continue;
            }
            if p == ".." {
                cur.pop();
                continue;
            }
            if p == "~" {
                cur.clear();
                continue;
            }
            let key = p.strip_suffix('/').unwrap_or(p);
            let cand = self.candidates(&cur);
            let mut hit: Option<String> = None;
            for c in &cand {
                if Self::plain(c) == key || c == p {
                    hit = Some(Self::plain(c));
                    break;
                }
            }
            match hit {
                Some(h) => cur.push(h),
                None => {
                    let close: Vec<&str> = cand
                        .iter()
                        .filter(|c| {
                            c.to_lowercase().contains(&key.to_lowercase())
                        })
                        .take(5)
                        .map(|s| s.as_str())
                        .collect();
                    return Err(format!(
                        "no such path: {} (similar: {})",
                        p,
                        if close.is_empty() {
                            "none".to_string()
                        } else {
                            close.join(", ")
                        }
                    ));
                }
            }
        }
        Ok(cur)
    }

    pub fn current_course(&self) -> Option<&Course> {
        self.pos.get(1).and_then(|k| self.courses.get(k))
    }

    pub fn current_module(&self) -> Option<&Module> {
        if self.pos.len() < 3 {
            return None;
        }
        let c = self.current_course()?;
        c.modules
            .iter()
            .find(|m| slug(&m.name, 120) == self.pos[2])
    }

    pub fn current_tab(&self) -> Option<Tab> {
        self.pos.first().and_then(|t| Tab::parse(t))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn mod_named(name: &str) -> Module {
        Module::new(&json!({"id": name, "name": name, "is_open": 1}))
    }

    #[test]
    fn slug_sanitizes() {
        assert_eq!(slug("a/b:c*d?\"e<f>g|h", 120), "a_b_c_d_e_f_g_h");
        assert_eq!(slug("  hi   there  ", 120), "hi there");
    }

    #[test]
    fn item_viewer_label() {
        let it = Item::new(json!({"id":"9","name":"kuis-1","contents_type":"url","content":"https://x"}));
        assert!(it.is_viewer_hint());
        assert!(it.label().ends_with("[viewer]"));
    }

    #[test]
    fn resolve_paths() {
        let mut v = Vfs::default();
        let mut c = Course::new(json!({"id":"1","name":"Aljabar Linear","code":"MA1101"}));
        c.modules = vec![mod_named("Minggu-01"), mod_named("Minggu-02")];
        let key = c.name.clone();
        v.courses.insert(key.clone(), c);
        assert_eq!(v.resolve("/").unwrap(), Vec::<String>::new());
        assert_eq!(
            v.ls(),
            vec!["active/".to_string(), "inactive/".to_string(), "all/".to_string()]
        );
        assert_eq!(
            v.resolve("active").unwrap(),
            vec!["active".to_string()]
        );
        v.pos = vec!["active".to_string(), key.clone()];
        assert_eq!(
            v.resolve("Minggu-01").unwrap(),
            vec!["active".to_string(), key.clone(), "Minggu-01".to_string()]
        );
        assert_eq!(v.resolve("../..").unwrap(), Vec::<String>::new());
        assert!(v.resolve("Typo").is_err());
    }

    #[test]
    fn ls_shapes() {
        let mut v = Vfs::default();
        let mut c = Course::new(json!({"id":"1","name":"Fisika","code":"FI1101"}));
        let mut m = mod_named("Tugas");
        m.items = vec![
            Item::new(json!({"id":"a","name":"slide.pdf","contents_type":"pdf","content":"https://cdn/x.pdf"})),
            Item::new(json!({"id":"b","name":"kuis-1","contents_type":"url","content":"https://x"})),
        ];
        c.modules = vec![m];
        let key = c.name.clone();
        v.courses.insert(key.clone(), c);
        v.pos = vec!["active".to_string()];
        assert_eq!(v.ls(), vec![format!("{key}/")]);
        v.pos = vec!["active".to_string(), key.clone()];
        assert_eq!(v.ls(), vec!["Tugas/".to_string()]);
        v.pos = vec!["active".to_string(), key, "Tugas".to_string()];
        let got = v.ls();
        assert!(got.iter().any(|g| g.contains("slide.pdf")));
        assert!(got.iter().any(|g| g.contains("[viewer]")));
    }

    #[test]
    fn tab_parse_and_qs() {
        assert_eq!(Tab::parse("active"), Some(Tab::Active));
        assert_eq!(Tab::parse("Inactive"), Some(Tab::Inactive));
        assert_eq!(Tab::parse("all"), Some(Tab::All));
        assert_eq!(Tab::parse("nope"), None);
        assert_eq!(Tab::Active.qs(), Some(("filter[is_active][is]", "1")));
        assert_eq!(Tab::Inactive.qs(), Some(("filter[is_active][is]", "0")));
        assert_eq!(Tab::All.qs(), None);
    }
}
