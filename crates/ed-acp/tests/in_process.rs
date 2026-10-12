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

mod provider_auth {
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::sync::{Arc, Mutex};

    use agent_client_protocol::schema::ProtocolVersion;
    use agent_client_protocol::schema::v1::{
        AuthMethod, AuthMethodAgent, AuthenticateRequest, InitializeRequest, LogoutRequest,
        NewSessionRequest,
    };
    use agent_client_protocol::{Agent, Channel, Client, ConnectionTo};
    use async_trait::async_trait;
    use ed_acp::{
        AgentInfo, AuthCtx, AuthProvider, LlmConfig, LlmEnv, Profile, PromptCtx, Provider,
        ServeOptions, Toolset,
    };

    /// A provider whose "session" is a string it holds in memory.
    #[derive(Default)]
    struct State {
        token: Mutex<Option<String>>,
        authenticated: AtomicUsize,
    }

    struct Fake(Arc<State>);

    #[async_trait]
    impl AuthProvider for Fake {
        fn method(&self) -> AuthMethod {
            AuthMethod::Agent(AuthMethodAgent::new("fake", "Sign in to Fake"))
        }

        fn llm_config(&self) -> Option<LlmConfig> {
            self.0.token.lock().unwrap().clone().map(|token| LlmConfig {
                provider: Provider::OpenAi,
                // Nothing listens here: listing models fails fast and the default is offered.
                base_url: "http://127.0.0.1:1".into(),
                api_key: Some(token),
                model: "fake-model".into(),
            })
        }

        async fn authenticate(&self, ctx: AuthCtx<'_>) -> agent_client_protocol::Result<()> {
            assert!(!ctx.client_offers_url_elicitation());
            self.0.authenticated.fetch_add(1, Ordering::SeqCst);
            *self.0.token.lock().unwrap() = Some("token".into());
            Ok(())
        }

        async fn logout(&self) -> anyhow::Result<()> {
            *self.0.token.lock().unwrap() = None;
            Ok(())
        }
    }

    struct WithAuth(Arc<State>);

    impl Profile for WithAuth {
        fn info(&self) -> AgentInfo {
            AgentInfo {
                name: "provider-agent",
                display_name: "Provider",
                version: "0.0.1",
                env: LlmEnv {
                    prefix: "ED_ACP_PROVIDER_TEST",
                    dir_name: "ed-acp-provider-test",
                    default_model: "fake-model",
                },
            }
        }

        fn system_prompt(&self, _ctx: &PromptCtx<'_>) -> String {
            String::new()
        }

        fn toolsets(&self) -> Vec<Arc<dyn Toolset>> {
            Vec::new()
        }

        fn auth(&self) -> Option<Arc<dyn AuthProvider>> {
            Some(Arc::new(Fake(self.0.clone())))
        }
    }

    #[tokio::test]
    async fn the_provider_owns_initialize_sessions_authenticate_and_logout() {
        let fake = Arc::new(State::default());
        let (agent_end, client_end) = Channel::duplex();
        let opts = ServeOptions {
            yolo: false,
            surface: "app",
        };
        tokio::spawn(ed_acp::serve(
            Arc::new(WithAuth(fake.clone())),
            opts,
            agent_end,
        ));
        let cwd = std::env::temp_dir();

        Client
            .builder()
            .connect_with(client_end, |conn: ConnectionTo<Agent>| async move {
                // The client cannot run terminal auth, and the method is offered anyway.
                let init = conn
                    .send_request(InitializeRequest::new(ProtocolVersion::V1))
                    .block_task()
                    .await?;
                assert_eq!(init.auth_methods.len(), 1);
                assert_eq!(&*init.auth_methods[0].id().0, "fake");

                let signed_out = conn
                    .send_request(NewSessionRequest::new(cwd.clone()))
                    .block_task()
                    .await;
                assert!(signed_out.is_err(), "no session while signed out");

                let wrong = conn
                    .send_request(AuthenticateRequest::new("terminal-setup"))
                    .block_task()
                    .await;
                assert!(wrong.is_err(), "only the provider's method is accepted");

                conn.send_request(AuthenticateRequest::new("fake"))
                    .block_task()
                    .await?;
                conn.send_request(NewSessionRequest::new(cwd.clone()))
                    .block_task()
                    .await?;

                conn.send_request(LogoutRequest::new()).block_task().await?;
                let after = conn
                    .send_request(NewSessionRequest::new(cwd.clone()))
                    .block_task()
                    .await;
                assert!(after.is_err(), "signed out again");
                Ok(())
            })
            .await
            .expect("conversation");

        assert_eq!(fake.authenticated.load(Ordering::SeqCst), 1);
    }
}
