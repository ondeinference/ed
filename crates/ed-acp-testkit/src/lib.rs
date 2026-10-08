//! Test support for Onde ACP agents: a mock Onde Cloud endpoint ([`mock`]), a raw JSON-RPC
//! client that drives the real agent binary over stdio ([`Raw`]), the ACP registry's validator
//! request ([`REGISTRY_VALIDATOR_INITIALIZE`]),
//! and the ACP v1 conformance suite ([`conformance`]).
//!
//! The test MCP server lives in `ed-mcp` (feature `test-server`) because it must be a binary,
//! and binaries cannot use dev-dependencies. An agent adopts the whole suite with one macro call
//! in an integration test:
//!
//! ```ignore
//! fn target() -> ed_acp_testkit::Target {
//!     ed_acp_testkit::Target {
//!         bin: env!("CARGO_BIN_EXE_my-agent").into(),
//!         name: "my-agent",
//!         env_prefix: "MY_AGENT",
//!         mcp_stub: env!("CARGO_BIN_EXE_mcp_stdio_stub").into(),
//!         base_tool_count: 5,
//!         env: vec![],
//!     }
//! }
//! ed_acp_testkit::conformance_suite!(target);
//! ```

pub mod conformance;
pub mod mock;
mod raw;

use std::path::{Path, PathBuf};

use serde_json::{Value, json};

pub use mock::{MODEL, MockStats, Script, Step, TEST_KEY, mock, start_mock_llm, temp_dir};
pub use raw::{
    Env, Perm, Raw, TIMEOUT, alive, error_code, read_pid, stub_server, tool_text, wait_dead,
};

/// The agent under test.
#[derive(Debug, Clone)]
pub struct Target {
    /// The agent binary, usually `env!("CARGO_BIN_EXE_<name>")`.
    pub bin: PathBuf,
    /// Expected `agentInfo.name`.
    pub name: &'static str,
    /// The agent's environment prefix: the suite sets `<PREFIX>_DATA_DIR`, `<PREFIX>_BASE_URL`
    /// and `<PREFIX>_MODEL`.
    pub env_prefix: &'static str,
    /// A binary whose `main` runs `ed_mcp::test_server::run`.
    pub mcp_stub: PathBuf,
    /// How many tools the agent offers with no MCP server connected.
    pub base_tool_count: usize,
    /// Extra environment for every run, e.g. switching built-in MCP servers off.
    pub env: Vec<(String, String)>,
}

impl Target {
    /// The environment for one run: an isolated data dir, the mock endpoint at `base_url`, the
    /// test key, and `PATH` set to the data dir so no stray binaries are found.
    pub fn env(&self, data_dir: &Path, base_url: &str) -> Env {
        let mut e = Env {
            bin: self.bin.clone(),
            prefix: self.env_prefix,
            vars: Vec::new(),
        };
        let (data, url, model) = (e.var("DATA_DIR"), e.var("BASE_URL"), e.var("MODEL"));
        e.set(&data, data_dir.display().to_string());
        e.set(&url, base_url);
        e.set(&model, MODEL);
        e.set("ONDE_API_KEY", TEST_KEY);
        e.set("PATH", data_dir.display().to_string());
        for (k, v) in &self.env {
            e.set(k, v.clone());
        }
        e
    }
}

/// The `initialize` params the ACP registry's validator sends, copied from
/// `agentclientprotocol/registry` `.github/workflows/client.py`. It signals terminal-auth
/// support only through `_meta`, never through `auth.terminal`.
pub fn registry_validator_initialize() -> Value {
    json!({
        "protocolVersion": 1,
        "clientInfo": {"name": "ACP Registry Validator", "version": "1.0.0"},
        "clientCapabilities": {
            "terminal": true,
            "fs": {"readTextFile": true, "writeTextFile": true},
            "_meta": {"terminal_output": true, "terminal-auth": true}
        }
    })
}

/// See [`registry_validator_initialize`].
pub const REGISTRY_VALIDATOR_INITIALIZE: &str = r#"{"protocolVersion":1,"clientInfo":{"name":"ACP Registry Validator","version":"1.0.0"},"clientCapabilities":{"terminal":true,"fs":{"readTextFile":true,"writeTextFile":true},"_meta":{"terminal_output":true,"terminal-auth":true}}}"#;

/// Generate one `#[tokio::test]` per conformance check, in a module named `acp_conformance`.
/// `$target` names a function, in scope where the macro is called, that returns the [`Target`].
#[macro_export]
macro_rules! conformance_suite {
    ($target:ident) => {
        mod acp_conformance {
            $crate::conformance_suite!(@tests $target;
                initialize_negotiates_v1_and_advertises_capabilities,
                registry_validator_probe_sees_terminal_auth,
                auth_required_then_authenticate_reloads_config,
                sessions_survive_a_killed_process_and_replay_tool_output,
                resume_close_delete_and_unknown_sessions,
                streaming_reports_message_ids_usage_and_inlines_file_links,
                permissions_reject_always_raw_output_and_auto_approve,
                auto_approve_is_hidden_from_clients_without_boolean_options,
                content_filter_is_a_refusal_and_private_flags_stay_off_the_wire,
                client_supplied_mcp_server_is_offered_gated_and_streams_progress,
                mcp_children_die_when_the_agent_exits,
                broken_mcp_server_is_not_fatal,
                cancelling_a_turn_cancels_the_mcp_call,
            );
        }
    };
    (@tests $target:ident; $($name:ident),* $(,)?) => {
        $(
            #[tokio::test]
            async fn $name() {
                $crate::conformance::$name(&super::$target()).await;
            }
        )*
    };
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_constant_and_the_value_agree() {
        let parsed: Value = serde_json::from_str(REGISTRY_VALIDATOR_INITIALIZE).unwrap();
        assert_eq!(parsed, registry_validator_initialize());
    }

    #[test]
    fn env_uses_the_prefix_and_target_overrides() {
        let t = Target {
            bin: "/bin/agent".into(),
            name: "agent",
            env_prefix: "MY",
            mcp_stub: "/bin/stub".into(),
            base_tool_count: 0,
            env: vec![("MY_MODEL".into(), "custom".into())],
        };
        let e = t.env(Path::new("/data"), "http://x/v1");
        let get = |k: &str| e.vars.iter().find(|(n, _)| n == k).map(|(_, v)| v.as_str());
        assert_eq!(get("MY_DATA_DIR"), Some("/data"));
        assert_eq!(get("MY_BASE_URL"), Some("http://x/v1"));
        assert_eq!(get("MY_MODEL"), Some("custom"));
        assert_eq!(get("ONDE_API_KEY"), Some(TEST_KEY));
    }
}
