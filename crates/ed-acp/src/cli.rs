//! Command-line pieces every Onde agent shares: `--setup` (choose a provider, store and verify
//! its key), `--list-models`, flag parsing and logging.

use std::io::IsTerminal;

use anyhow::Context;

use crate::AgentInfo;
use crate::llm::{
    LOCAL_DEFAULT_MODEL, LlmClient, LlmConfig, LlmEnv, OPENAI_DEFAULT_BASE_URL,
    OPENAI_DEFAULT_MODEL, Provider,
};

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
    with_vars(existing, &[("ONDE_API_KEY".to_string(), key.to_string())])
}

/// `env` file content with each of `vars` set, replacing earlier values and keeping any
/// other lines.
pub fn with_vars(existing: &str, vars: &[(String, String)]) -> String {
    let mut lines: Vec<String> = existing
        .lines()
        .filter(|l| {
            let l = l.trim().strip_prefix("export ").unwrap_or(l.trim());
            !vars.iter().any(|(k, _)| l.starts_with(&format!("{k}=")))
        })
        .map(String::from)
        .collect();
    lines.extend(vars.iter().map(|(k, v)| format!("{k}={v}")));
    lines.join("\n") + "\n"
}

/// Interactive first-run setup (`--setup`): choose Onde Inference or an OpenAI API compatible
/// endpoint, prompt for its key, verify it, and store it in the platform config dir
/// (`config_dir()/env`, mode 0600).
pub async fn setup(info: &AgentInfo) -> anyhow::Result<()> {
    use std::io::Write;

    if !std::io::stdin().is_terminal() {
        anyhow::bail!("--setup needs an interactive terminal");
    }
    let prompt_line = |label: &str| -> anyhow::Result<String> {
        print!("{label}");
        std::io::stdout().flush()?;
        let mut line = String::new();
        if std::io::stdin().read_line(&mut line)? == 0 {
            anyhow::bail!("no input");
        }
        Ok(line.trim().to_string())
    };

    println!("{} setup\n", info.name);
    println!("  1) Onde Inference                    (ONDE_API_KEY)");
    println!(
        "  2) OpenAI API compatible endpoint    (OPENAI_BASE_URL, OPENAI_API_KEY, OPENAI_MODEL)"
    );
    let local = cfg!(feature = "local");
    if local {
        println!("  3) On-device                          (runs here, no key; downloads a model)");
    }
    let last = if local { 3 } else { 2 };
    let provider = loop {
        match prompt_line(&format!("\nChoose a provider [1-{last}]: "))?.as_str() {
            "" | "1" | "onde" => break Provider::Onde,
            "2" | "openai" => break Provider::OpenAi,
            "3" | "local" if local => break Provider::Local,
            _ => println!("Please enter a number from 1 to {last}."),
        }
    };
    let mut vars = vec![(info.env.var("PROVIDER"), provider.name().to_string())];
    match provider {
        Provider::Onde => {
            println!("\nGet credentials: sign in at https://ondeinference.com/root/login,");
            println!("register an app and assign a model. Your key is \"app-id:app-secret\".\n");
        }
        // A generic endpoint also needs its URL and a model that supports tool calling.
        Provider::OpenAi => {
            for (var, label, default) in [
                ("OPENAI_BASE_URL", "Base URL", OPENAI_DEFAULT_BASE_URL),
                ("OPENAI_MODEL", "Model", OPENAI_DEFAULT_MODEL),
            ] {
                let value = prompt_line(&format!("{label} [{default}]: "))?;
                let value = if value.is_empty() {
                    default.to_string()
                } else {
                    value
                };
                vars.push((var.to_string(), value));
            }
        }
        Provider::Local => {
            println!(
                "\nThe model ({}) downloads the first time the agent needs it.",
                LOCAL_DEFAULT_MODEL
            );
            println!("Set {} to pick another one.", info.env.var("MODEL"));
            return write_env(info, &vars);
        }
    }
    let key_var = provider.key_var();
    let key = loop {
        let key = prompt_line(&format!("Paste your {key_var}: "))?;
        match provider {
            Provider::Onde if !valid_onde_key(&key) => {
                println!("Onde credentials look like \"app-id:app-secret\" (exactly one colon).")
            }
            _ if key.is_empty() => println!("Key must not be empty."),
            _ => break key,
        }
    };
    vars.push((key_var.to_string(), key));

    // Verify before writing anything: the new values win over the environment.
    let config = LlmConfig::from_lookup(&info.env, |name| {
        vars.iter()
            .find(|(k, _)| k == name)
            .map(|(_, v)| v.clone())
            .or_else(|| std::env::var(name).ok())
    });
    print!("Verifying the key with {}… ", provider.display_name());
    std::io::stdout().flush()?;
    match LlmClient::new(config).check_auth().await {
        Ok(()) => println!("OK"),
        Err(e) => anyhow::bail!("\nKey check failed: {e:#}\nNothing was written."),
    }

    write_env(info, &vars)
}

/// Store `vars` in the agent's `env` file (mode 0600), keeping its other lines.
fn write_env(info: &AgentInfo, vars: &[(String, String)]) -> anyhow::Result<()> {
    let dir = info
        .env
        .config_dir()
        .context("no config directory available")?;
    std::fs::create_dir_all(&dir)?;
    let path = dir.join("env");
    let existing = std::fs::read_to_string(&path).unwrap_or_default();
    std::fs::write(&path, with_vars(&existing, vars))?;
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
        let vars = [
            ("X_PROVIDER".to_string(), "openai".to_string()),
            ("OPENAI_API_KEY".to_string(), "sk".to_string()),
        ];
        assert_eq!(
            with_vars("X_PROVIDER=onde\nONDE_API_KEY=a:b\n", &vars),
            "ONDE_API_KEY=a:b\nX_PROVIDER=openai\nOPENAI_API_KEY=sk\n"
        );
    }
}
