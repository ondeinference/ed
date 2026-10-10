//! Durable sessions: one JSON file per session under `<data_dir>/sessions/`.
//!
//! The history stored here is the OpenAI-style message list without the system prompt
//! (that is rebuilt every turn). Tool results are `role: "tool"` messages, which is what
//! lets `session/load` replay tool calls with their output.

use std::path::{Path, PathBuf};

use anyhow::{Context, Result, bail};
use serde::{Deserialize, Serialize};
use serde_json::Value;

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct StoredSession {
    pub cwd: PathBuf,
    #[serde(default)]
    pub roots: Vec<PathBuf>,
    pub model: String,
    #[serde(default)]
    pub title: Option<String>,
    pub updated_at: u64,
    #[serde(default)]
    pub messages: Vec<Value>,
}

#[derive(Debug, Clone)]
pub struct SessionStore {
    dir: PathBuf,
}

/// Session ids end up in file names: allow only characters that cannot escape the directory.
pub fn sanitize_id(id: &str) -> Result<&str> {
    let ok = !id.is_empty()
        && id.len() <= 128
        && id
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_');
    if !ok {
        bail!("invalid session id");
    }
    Ok(id)
}

impl SessionStore {
    pub fn new(data_dir: &Path) -> Self {
        Self {
            dir: data_dir.join("sessions"),
        }
    }

    fn path(&self, id: &str) -> Result<PathBuf> {
        Ok(self.dir.join(format!("{}.json", sanitize_id(id)?)))
    }

    /// Atomic write: temp file in the same directory, then rename.
    pub fn save(&self, id: &str, session: &StoredSession) -> Result<()> {
        let path = self.path(id)?;
        std::fs::create_dir_all(&self.dir)
            .with_context(|| format!("creating {}", self.dir.display()))?;
        let tmp = self.dir.join(format!(".{id}.{}.tmp", uuid::Uuid::new_v4()));
        std::fs::write(&tmp, serde_json::to_vec(session)?)?;
        std::fs::rename(&tmp, &path).inspect_err(|_| {
            let _ = std::fs::remove_file(&tmp);
        })?;
        Ok(())
    }

    /// `Ok(None)` when no such session exists (or the id is not a valid one).
    pub fn load(&self, id: &str) -> Result<Option<StoredSession>> {
        let Ok(path) = self.path(id) else {
            return Ok(None);
        };
        match std::fs::read(&path) {
            Ok(bytes) => Ok(Some(
                serde_json::from_slice(&bytes)
                    .with_context(|| format!("parsing {}", path.display()))?,
            )),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
            Err(e) => Err(e).with_context(|| format!("reading {}", path.display())),
        }
    }

    /// All stored sessions, newest first. Unreadable files are skipped.
    pub fn list(&self) -> Vec<(String, StoredSession)> {
        let Ok(rd) = std::fs::read_dir(&self.dir) else {
            return Vec::new();
        };
        let mut out: Vec<(String, StoredSession)> = rd
            .filter_map(|e| {
                let path = e.ok()?.path();
                if path.extension()? != "json" {
                    return None;
                }
                let id = path.file_stem()?.to_str()?.to_string();
                let session = serde_json::from_slice(&std::fs::read(&path).ok()?).ok()?;
                Some((id, session))
            })
            .collect();
        out.sort_by(|a, b| b.1.updated_at.cmp(&a.1.updated_at).then(a.0.cmp(&b.0)));
        out
    }

    /// Returns whether a file was removed.
    pub fn delete(&self, id: &str) -> Result<bool> {
        let Ok(path) = self.path(id) else {
            return Ok(false);
        };
        match std::fs::remove_file(&path) {
            Ok(()) => Ok(true),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(false),
            Err(e) => Err(e).with_context(|| format!("deleting {}", path.display())),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn tmp() -> PathBuf {
        let d = std::env::temp_dir().join(format!("sf-store-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&d).unwrap();
        d
    }

    fn sample(updated_at: u64) -> StoredSession {
        StoredSession {
            cwd: "/work".into(),
            roots: vec!["/other".into()],
            model: "m".into(),
            title: Some("hello".into()),
            updated_at,
            messages: vec![
                json!({"role": "user", "content": "hi"}),
                json!({"role": "tool", "tool_call_id": "c1", "content": "out"}),
            ],
        }
    }

    #[test]
    fn round_trip_list_delete() {
        let dir = tmp();
        let store = SessionStore::new(&dir);
        assert!(store.load("a").unwrap().is_none());
        store.save("a", &sample(10)).unwrap();
        store.save("b", &sample(20)).unwrap();
        assert_eq!(store.load("a").unwrap().unwrap(), sample(10));
        let ids: Vec<_> = store.list().into_iter().map(|(id, _)| id).collect();
        assert_eq!(ids, ["b", "a"]);
        assert!(store.delete("a").unwrap());
        assert!(!store.delete("a").unwrap());
        assert!(store.load("a").unwrap().is_none());
        // No temp files left behind.
        let leftovers = std::fs::read_dir(dir.join("sessions"))
            .unwrap()
            .filter(|e| e.as_ref().unwrap().path().extension().unwrap() != "json")
            .count();
        assert_eq!(leftovers, 0);
        std::fs::remove_dir_all(dir).ok();
    }

    #[test]
    fn rejects_path_traversal_ids() {
        let dir = tmp();
        let store = SessionStore::new(&dir);
        for bad in ["../x", "a/b", "", "a.b", "..", "a\\b"] {
            assert!(store.save(bad, &sample(1)).is_err(), "{bad}");
            assert!(store.load(bad).unwrap().is_none());
            assert!(!store.delete(bad).unwrap());
        }
        std::fs::remove_dir_all(dir).ok();
    }
}
