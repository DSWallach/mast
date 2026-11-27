//! mast-mcp — a Model Context Protocol server over the running engine.
//!
//! `mast mcp` speaks MCP (JSON-RPC 2.0, newline-delimited) on stdin/stdout
//! and proxies every tool into the engine's localhost IPC — the same
//! `mast_rpc` WebSocket the headed app and `mast sync` dial. Nothing here
//! talks to the edge or touches the filesystem: the engine stays the single
//! authority for chats, devices, projects, and the command plane.
//!
//! The server is hand-rolled rather than an SDK: the MCP surface a stdio
//! tool server needs is five methods (`initialize`, `ping`, `tools/list`,
//! `tools/call`, and the `initialized` notification), and the repo already
//! owns JSON-RPC framing for the Codex and ACP drivers. Zero new crates.
//!
//! When the engine launches a Run of a Chat in a Space on Claude Code or
//! Codex it injects this server with an engine-minted per-Run capability
//! token in `MAST_AGENT_TOKEN`. Every tool call re-authenticates the token
//! with the engine (`AgentAuthenticate`), which answers with the Chat the
//! token was minted for; nothing self-declared is trusted. Such an agent also
//! gets the Work Outline agent tools (`AgentToolCall`), its sends are
//! attributed to its Chat, a Chat can never message itself, `create_chat`
//! requires an idempotency key, and the original tools are scoped to its
//! Chat (and the Chats it created), project, and device. Without a token (a human running `mast mcp`
//! in a terminal) the original unattributed tools work as before.

mod jsonrpc;
mod mast;
mod peer_git;
mod tools;
mod transcript;

pub use jsonrpc::serve_stdio;
pub use mast::{Mast, token_from_env};
pub use tools::{ToolDef, Tools};
pub use transcript::{RenderOptions, RenderedMessage, render_entries};

/// How `mast mcp` finds the engine and which Run's token it presents.
#[derive(Clone)]
pub struct McpConfig {
    /// Loopback IPC port of the engine to proxy (`MAST_IPC_PORT`, default 27654).
    pub ipc_port: u16,
    /// The engine-minted per-Run token (`MAST_AGENT_TOKEN`), when injected.
    pub token: Option<String>,
    /// A paired peer's listener (`MAST_PEER_URL`) and the bearer it issued at
    /// pairing (`MAST_PEER_TOKEN`). When set, this server proxies that engine
    /// instead of the local one and presents no Run token.
    pub peer: Option<(String, String)>,
}

impl std::fmt::Debug for McpConfig {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("McpConfig")
            .field("ipc_port", &self.ipc_port)
            .field("token", &self.token.as_ref().map(|_| "<redacted>"))
            .field("peer", &self.peer.as_ref().map(|(url, _)| url))
            .finish()
    }
}

impl McpConfig {
    /// Resolve from the process environment: `MAST_IPC_PORT` for the engine,
    /// `MAST_AGENT_TOKEN` for the Run. `MAST_CHAT_ID` is ignored.
    pub fn from_env() -> Self {
        let ipc_port = mast_proto::env::var(mast_proto::agent_tools::IPC_PORT_ENV)
            .ok()
            .and_then(|p| p.parse().ok())
            .unwrap_or(27654);
        let peer = match (
            mast_proto::env::var("MAST_PEER_URL").ok(),
            mast_proto::env::var("MAST_PEER_TOKEN").ok(),
        ) {
            (Some(url), Some(token)) if !url.trim().is_empty() && !token.trim().is_empty() => {
                Some((url.trim().to_string(), token.trim().to_string()))
            }
            _ => None,
        };
        Self {
            ipc_port,
            token: if peer.is_some() { None } else { token_from_env() },
            peer,
        }
    }
}

/// Run the MCP server on this process's stdin/stdout until stdin closes.
pub async fn run(config: McpConfig) -> anyhow::Result<()> {
    let zeron = match config.peer {
        Some((url, bearer)) => Mast::new_peer(url, bearer),
        None => Mast::new(format!("ws://127.0.0.1:{}", config.ipc_port), config.token),
    };
    let tools = Tools::new(std::sync::Arc::new(zeron));
    serve_stdio(std::sync::Arc::new(tools)).await
}

#[cfg(test)]
mod env_tests {
    use super::*;
    use std::sync::Arc;

    static ENV_LOCK: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());

    struct EnvReset;

    impl EnvReset {
        fn clear() {
            unsafe {
                for key in [
                    "MAST_IPC_PORT",
                    "MAST_AGENT_TOKEN",
                    "MAST_PEER_URL",
                    "MAST_PEER_TOKEN",
                    "ZERON_IPC_PORT",
                    "ZERON_AGENT_TOKEN",
                    "ZERON_PEER_URL",
                    "ZERON_PEER_TOKEN",
                ] {
                    std::env::remove_var(key);
                }
            }
        }
    }

    impl Drop for EnvReset {
        fn drop(&mut self) {
            Self::clear();
        }
    }

    /// Standalone Mast 0.3.0 ignores `ZERON_*` env variables and reads only
    /// `MAST_*` variables.
    #[tokio::test]
    async fn zeron_env_names_are_ignored_and_mast_env_names_configure_the_server() {
        let _guard = ENV_LOCK.lock().await;
        EnvReset::clear();
        let _reset = EnvReset;

        unsafe {
            std::env::set_var("ZERON_IPC_PORT", "27999");
            std::env::set_var("ZERON_AGENT_TOKEN", " zat_old ");
            std::env::set_var("ZERON_PEER_URL", "ws://127.0.0.1:28000");
            std::env::set_var("ZERON_PEER_TOKEN", "peer_old");
        }
        let config = McpConfig::from_env();
        assert_eq!(config.ipc_port, 27654);
        assert_eq!(config.token, None);
        assert_eq!(config.peer, None);

        unsafe {
            std::env::set_var("MAST_IPC_PORT", "27999");
            std::env::set_var("MAST_AGENT_TOKEN", " zat_new ");
        }
        let config = McpConfig::from_env();
        assert_eq!(config.ipc_port, 27999);
        assert_eq!(config.token.as_deref(), Some("zat_new"));
        assert_eq!(config.peer, None);
    }

    /// Hosted agent MCP calls authenticate over loopback IPC using
    /// `MAST_IPC_PORT` and `MAST_AGENT_TOKEN`, ignoring any `ZERON_*` values.
    #[tokio::test(flavor = "multi_thread")]
    async fn hosted_agent_mcp_call_works_with_mast_env() {
        let _guard = ENV_LOCK.lock().await;
        EnvReset::clear();
        let _reset = EnvReset;

        let tmp = tempfile::tempdir().unwrap();
        let root = std::fs::canonicalize(tmp.path()).unwrap();
        let project = root.join("project");
        std::fs::create_dir_all(&project).unwrap();
        let core = mast_engine::EngineCore::assemble(
            &root.join("data"),
            Arc::new(mast_engine::HarnessRegistry::new()),
            mast_proto::HarnessId::Mock,
            None,
        )
        .expect("engine core assembles");

        let admin = mast_rpc::memory_client(core.rpc_service());
        admin
            .call(
                mast_rpc::methods::MUTATE,
                serde_json::json!({
                    "op": "createSpace",
                    "spaceId": "space-1",
                    "deviceId": core.device_id,
                    "path": project.to_string_lossy(),
                }),
            )
            .await
            .unwrap();
        admin
            .call(
                mast_rpc::methods::MUTATE,
                serde_json::json!({
                    "op": "createChat",
                    "chatId": "chat-1",
                    "spaceId": "space-1",
                    "deviceId": core.device_id,
                    "config": {
                        "harness": "mock",
                        "model": null,
                        "reasoning": null,
                        "sandbox": "workspace-write",
                    },
                }),
            )
            .await
            .unwrap();

        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let port = listener.local_addr().unwrap().port();
        let server = tokio::spawn(mast_rpc::serve_ws_listener(listener, core.rpc_service()));

        let lease = core
            .sessions
            .agent_tokens()
            .mint("chat-1", "run-1", "space-1", &core.device_id);
        let launch = mast_engine::agent_tools::AgentToolsLaunch {
            command: std::path::PathBuf::from("mast"),
            args: vec!["mcp".into()],
            ipc_port: Some(port),
        };
        let injection = launch.injection(lease.token()).unwrap();
        assert_eq!(injection.env.len(), 2);
        assert!(!injection.env.contains_key("ZERON_AGENT_TOKEN"));
        assert!(!injection.env.contains_key("ZERON_IPC_PORT"));

        unsafe {
            std::env::set_var("ZERON_IPC_PORT", "1");
            std::env::set_var("ZERON_AGENT_TOKEN", "zat_wrong");
            for (k, v) in &injection.env {
                std::env::set_var(k, v);
            }
        }
        let config = McpConfig::from_env();
        assert_eq!(config.ipc_port, port);
        assert_eq!(config.token.as_deref(), Some(lease.token()));

        let mast = Mast::new(format!("ws://127.0.0.1:{}", config.ipc_port), config.token);
        let tools = Tools::new(Arc::new(mast));
        let whoami = tools.call("whoami", serde_json::json!({})).await.unwrap();
        assert_eq!(whoami["authenticated"], true);
        assert_eq!(whoami["chat"]["id"], "chat-1");
        assert_eq!(whoami["runId"], "run-1");

        server.abort();
    }
}

#[cfg(test)]
#[path = "../../../test_support/isolation.rs"]
mod test_isolation;
