//! Command-line pieces every Onde agent shares: `--setup` (store and verify the Onde key),
//! `--list-models`, flag parsing and logging.

use std::io::IsTerminal;

use anyhow::Context;

use crate::AgentInfo;
use crate::llm::{LlmClient, LlmConfig, LlmEnv};

/// Log to stderr, filtered by `RUST_LOG`. Stdout is reserved for ACP.
pub fn init_logging() {
    let _ = tracing_subscriber::fmt()
        .with_writer(std::io::stderr)
        .with_env_filter(tracing_subscriber::EnvFilter::from_default_env())
        .try_init();
}

/// `1` or `true` (any case).
pub fn env_flag(name: &str) -> bool {
    std::env::var(name).is_ok_and(|v| v == "1" || v.eq_ignore_ascii_case("true"))
}

/// Onde credentials are `app-id:app-secret`: exactly one colon, both halves non-empty.
pub fn valid_onde_key(key: &str) -> bool {
    let mut parts = key.split(':');
    matches!(
        (parts.next(), parts.next(), parts.next()),
        (Some(id), Some(secret), None) if !id.is_empty() && !secret.is_empty()
    )
}

/// `env` file content with `ONDE_API_KEY` set, keeping any other lines.
pub fn with_api_key(existing: &str, key: &str) -> String {
    let mut lines: Vec<String> = existing
        .lines()
        .filter(|l| {
            let l = l.trim().strip_prefix("export ").unwrap_or(l.trim());
            !l.starts_with("ONDE_API_KEY=")
        })
        .map(String::from)
        .collect();
    lines.push(format!("ONDE_API_KEY={key}"));
    lines.join("\n") + "\n"
}

/// Interactive first-run setup (`--setup`): prompt for the Onde key, verify it against Onde
/// Cloud, and store it in the platform config dir (`config_dir()/env`, mode 0600).
pub async fn setup(info: &AgentInfo) -> anyhow::Result<()> {
    use std::io::Write;

    if !std::io::stdin().is_terminal() {
        anyhow::bail!("--setup needs an interactive terminal");
    }
    println!("{} setup\n", info.name);
    println!("Get credentials: sign in at https://ondeinference.com/root/login,");
    println!("register an app and assign a model. Your key is \"app-id:app-secret\".\n");
    let key = loop {
        print!("Paste your ONDE_API_KEY: ");
        std::io::stdout().flush()?;
        let mut line = String::new();
        if std::io::stdin().read_line(&mut line)? == 0 {
            anyhow::bail!("no input");
        }
        let key = line.trim().to_string();
        if valid_onde_key(&key) {
            break key;
        }
        println!("Onde credentials look like \"app-id:app-secret\" (exactly one colon).");
    };

    // Verify before writing anything.
    let mut config = LlmConfig::from_env(&info.env);
    config.api_key = Some(key.clone());
    print!("Verifying the key with Onde Cloud… ");
    std::io::stdout().flush()?;
    match LlmClient::new(config).check_auth().await {
        Ok(()) => println!("OK"),
        Err(e) => anyhow::bail!("\nKey check failed: {e:#}\nNothing was written."),
    }

    let dir = info
        .env
        .config_dir()
        .context("no config directory available")?;
    std::fs::create_dir_all(&dir)?;
    let path = dir.join("env");
    let existing = std::fs::read_to_string(&path).unwrap_or_default();
    std::fs::write(&path, with_api_key(&existing, &key))?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600))?;
    }
    println!("\nWrote {}. You're all set.", path.display());
    Ok(())
}

/// Print the models the configured endpoint serves, one per line (`id<TAB>owner`).
pub async fn list_models(info: &AgentInfo) -> anyhow::Result<()> {
    let client = LlmClient::new(LlmConfig::from_env(&info.env));
    for m in client.models().await? {
        match m.owned_by {
            Some(owner) => println!("{}\t{}", m.id, owner),
            None => println!("{}", m.id),
        }
    }
    Ok(())
}

/// How the agent was launched: `tui` (interactive terminal UI) or `acp` (editor).
/// The TUI sets `<PREFIX>_SURFACE=tui` on its subprocess; editors launch `--acp`
/// directly so the default is `acp`.
pub fn surface(env: &LlmEnv) -> &'static str {
    match std::env::var(env.var("SURFACE")) {
        Ok(s) if s == "tui" => "tui",
        _ => "acp",
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn onde_key_shape() {
        assert!(valid_onde_key("app:secret"));
        assert!(!valid_onde_key("secret"));
        assert!(!valid_onde_key("a:b:c"));
        assert!(!valid_onde_key(":secret"));
        assert!(!valid_onde_key("app:"));
        assert!(!valid_onde_key(""));
    }

    #[test]
    fn env_file_keeps_other_lines_and_replaces_key() {
        let out = with_api_key("# hi\nX_MODEL=m\nexport ONDE_API_KEY=old:old\n", "new:key");
        assert_eq!(out, "# hi\nX_MODEL=m\nONDE_API_KEY=new:key\n");
        assert_eq!(with_api_key("", "a:b"), "ONDE_API_KEY=a:b\n");
    }
}
