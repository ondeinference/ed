//! `serve` over an in-memory channel: the agent and its client in one process, no stdio.

use std::sync::Arc;

use agent_client_protocol::schema::ProtocolVersion;
use agent_client_protocol::schema::v1::InitializeRequest;
use agent_client_protocol::{Agent, Channel, Client, ConnectionTo};
use ed_acp::{AgentInfo, LlmEnv, Profile, PromptCtx, ServeOptions, Toolset};

struct Embedded;

impl Profile for Embedded {
    fn info(&self) -> AgentInfo {
        AgentInfo {
            name: "embedded-agent",
            display_name: "Embedded",
            version: "0.0.1",
            env: LlmEnv {
                prefix: "ED_ACP_IN_PROCESS_TEST",
                dir_name: "ed-acp-in-process-test",
                default_model: "test-model",
            },
        }
    }

    fn system_prompt(&self, _ctx: &PromptCtx<'_>) -> String {
        String::new()
    }

    fn toolsets(&self) -> Vec<Arc<dyn Toolset>> {
        Vec::new()
    }
}

#[tokio::test]
async fn initialize_over_an_in_memory_channel() {
    let (agent_end, client_end) = Channel::duplex();
    let opts = ServeOptions {
        yolo: false,
        surface: "app",
    };
    let server = tokio::spawn(ed_acp::serve(Arc::new(Embedded), opts, agent_end));

    let info = Client
        .builder()
        .connect_with(client_end, |conn: ConnectionTo<Agent>| async move {
            conn.send_request(InitializeRequest::new(ProtocolVersion::V1))
                .block_task()
                .await
        })
        .await
        .expect("initialize");

    let agent = info.agent_info.expect("agentInfo");
    assert_eq!(agent.name, "embedded-agent");
    assert_eq!(agent.version, "0.0.1");
    let caps = info.agent_capabilities;
    assert!(caps.load_session);

    // The client hung up when `connect_with` returned, so the server stops on its own.
    tokio::time::timeout(std::time::Duration::from_secs(5), server)
        .await
        .expect("server stops after the client disconnects")
        .expect("server task")
        .ok();
}
