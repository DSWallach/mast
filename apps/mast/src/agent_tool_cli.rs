use std::sync::Arc;

use anyhow::{Context, ensure};
use mast_mcp::{Mast, McpConfig, Tools};
use serde_json::Value;

pub async fn run(tool: &str, payload: &str, config: McpConfig) -> anyhow::Result<()> {
    let arguments: Value = serde_json::from_str(payload).context("invalid --json payload")?;
    ensure!(arguments.is_object(), "--json payload must be an object");
    let mast = match config.peer {
        Some((url, bearer)) => Mast::new_peer(url, bearer),
        None => {
            ensure!(
                config.token.is_some(),
                "MAST_AGENT_TOKEN is required for local agent-tool calls"
            );
            Mast::new(format!("ws://127.0.0.1:{}", config.ipc_port), config.token)
        }
    };
    // MCP and CLI share the dispatcher. Engine-side tools derive their keys
    // after resolving scope and current state, just as they do over stdio.
    let result = Tools::new(Arc::new(mast))
        .call(tool, arguments)
        .await
        .map_err(anyhow::Error::msg)?;
    println!("{}", serde_json::to_string(&result)?);
    Ok(())
}
