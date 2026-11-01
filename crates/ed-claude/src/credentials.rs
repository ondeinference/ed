//! The stored Anthropic API key and chosen model.
//!
//! An `env`-style file (`ED_CLAUDE_API_KEY=...`, `ED_CLAUDE_MODEL=...`) at mode 0600 in the
//! agent's config dir. A host sets that dir once with [`set_dir`]; in a sandboxed app it is
//! inside the container, so the app never shares a key with anything outside it. The
//! functions here only read that file, never the process environment.

use std::path::{Path, PathBuf};
use std::sync::OnceLock;

use directories::BaseDirs;

use crate::profile::INFO;

const FILE: &str = "env";
const KEY_VAR: &str = "ED_CLAUDE_API_KEY";
const MODEL_VAR: &str = "ED_CLAUDE_MODEL";

static DIR: OnceLock<PathBuf> = OnceLock::new();

/// What is stored: the key, and the model chosen with it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Stored {
    pub api_key: String,
    pub model: Option<String>,
}

/// Keep the key in `dir` instead of the platform config dir. The first call wins.
pub fn set_dir(dir: PathBuf) {
    if let Err(rejected) = DIR.set(dir) {
        tracing::warn!(
            "ed-claude config dir is already {:?}; ignoring {rejected:?}",
            DIR.get()
        );
    }
}

/// The dir [`set_dir`] chose, else `<config dir>/edclaude`.
pub fn dir() -> Option<PathBuf> {
    if let Some(dir) = DIR.get() {
        return Some(dir.clone());
    }
    Some(BaseDirs::new()?.config_dir().join(INFO.env.dir_name))
}

/// Store `api_key`, and `model` when given, replacing what was there.
pub fn store_api_key(api_key: &str, model: Option<&str>) -> Result<(), String> {
    let dir = dir().ok_or("cannot resolve the config directory")?;
    store_in(&dir, api_key, model)
}

/// The stored key and model, or `None` when no key is stored.
pub fn load() -> Option<Stored> {
    load_from(&dir()?)
}

/// Delete the stored key. Returns `true` if a file was removed.
pub fn clear() -> bool {
    dir().is_some_and(|dir| clear_in(&dir))
}

/// For tests of this crate and its dependents: store into `dir` rather than the shared one.
#[doc(hidden)]
pub fn store_in_for_tests(dir: &Path, api_key: &str, model: Option<&str>) {
    store_in(dir, api_key, model).expect("store");
}

pub(crate) fn store_in(dir: &Path, api_key: &str, model: Option<&str>) -> Result<(), String> {
    let api_key = api_key.trim();
    if api_key.is_empty() || api_key.contains(['\n', '\r']) {
        return Err("the API key is empty or spans lines".to_string());
    }
    std::fs::create_dir_all(dir).map_err(|e| format!("create {dir:?}: {e}"))?;
    let mut body = format!("{KEY_VAR}={api_key}\n");
    if let Some(model) = model.map(str::trim).filter(|m| !m.is_empty()) {
        body.push_str(&format!("{MODEL_VAR}={model}\n"));
    }
    let path = dir.join(FILE);
    write_private(&path, &body).map_err(|e| format!("write {path:?}: {e}"))
}

pub(crate) fn load_from(dir: &Path) -> Option<Stored> {
    let text = std::fs::read_to_string(dir.join(FILE)).ok()?;
    let value = |name: &str| {
        text.lines()
            .find_map(|line| line.strip_prefix(name)?.strip_prefix('='))
            .map(str::trim)
            .filter(|v| !v.is_empty())
            .map(str::to_string)
    };
    Some(Stored {
        api_key: value(KEY_VAR)?,
        model: value(MODEL_VAR),
    })
}

pub(crate) fn clear_in(dir: &Path) -> bool {
    std::fs::remove_file(dir.join(FILE)).is_ok()
}

#[cfg(unix)]
fn write_private(path: &Path, contents: &str) -> std::io::Result<()> {
    use std::io::Write;
    use std::os::unix::fs::{OpenOptionsExt, PermissionsExt};
    // Created at 0600 so the key is never readable by others, even briefly.
    let mut file = std::fs::OpenOptions::new()
        .write(true)
        .create(true)
        .truncate(true)
        .mode(0o600)
        .open(path)?;
    file.set_permissions(std::fs::Permissions::from_mode(0o600))?;
    file.write_all(contents.as_bytes())
}

#[cfg(not(unix))]
fn write_private(path: &Path, contents: &str) -> std::io::Result<()> {
    std::fs::write(path, contents)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn scratch(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("ed-claude-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        dir
    }

    #[test]
    fn round_trips_the_key_and_model() {
        let dir = scratch("roundtrip");
        assert_eq!(load_from(&dir), None);
        store_in(&dir, "  sk-ant-1  ", Some("claude-opus-5-5")).unwrap();
        assert_eq!(
            load_from(&dir),
            Some(Stored {
                api_key: "sk-ant-1".into(),
                model: Some("claude-opus-5-5".into())
            })
        );
        store_in(&dir, "sk-ant-2", None).unwrap();
        assert_eq!(load_from(&dir).unwrap().model, None);
        assert!(clear_in(&dir));
        assert_eq!(load_from(&dir), None);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn rejects_a_key_that_would_corrupt_the_file() {
        let dir = scratch("reject");
        assert!(store_in(&dir, "", None).is_err());
        assert!(store_in(&dir, "a\nED_CLAUDE_MODEL=x", None).is_err());
    }

    #[cfg(unix)]
    #[test]
    fn the_file_is_private() {
        use std::os::unix::fs::PermissionsExt;
        let dir = scratch("mode");
        store_in(&dir, "sk-ant-1", None).unwrap();
        let mode = std::fs::metadata(dir.join(FILE))
            .unwrap()
            .permissions()
            .mode();
        assert_eq!(mode & 0o777, 0o600);
        let _ = std::fs::remove_dir_all(&dir);
    }
}
