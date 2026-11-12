//! The personal assistant's desktop management bridge. Never accepts arbitrary
//! RPC names: the shared, discoverable catalog is the authority for dispatch.
use super::*;
use mast_proto::{agent_tools::AgentToolCallParams, mast_management};

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct ManagementCall {
    method: Option<String>,
    #[serde(default = "empty_params")]
    params: serde_json::Value,
}

fn empty_params() -> serde_json::Value {
    serde_json::json!({})
}

impl EngineRpc {
    pub(super) async fn assistant_manage(
        &self,
        call: &AgentToolCallParams,
    ) -> Result<serde_json::Value, RpcError> {
        // This re-reads the designation on EVERY call, including catalog reads.
        // A removed assistant, an old run token, or an ordinary chat cannot use
        // the desktop's management authority.
        let origin = self
            .agent_tools
            .authenticate(&call.token)
            .map_err(|e| RpcError::Failed(e.to_wire_message()))?;
        if !origin.personal_assistant {
            return Err(RpcError::Failed(
                "unauthorized: mast_manage requires the user-designated personal assistant".into(),
            ));
        }
        let args: ManagementCall = parse_params(call.arguments.clone())?;
        let catalog = mast_management::operations();
        let Some(method) = args.method else {
            return Ok(catalog);
        };
        if catalog.get(&method).is_none() {
            return Err(RpcError::Failed(format!(
                "Unsupported management method {method}; call mast_manage with {{}} for available operations"
            )));
        }
        if !args.params.is_object() {
            return Err(RpcError::Failed("params must be an object".into()));
        }
        // Reuse the desktop handlers: revision checks, validation, persistence,
        // watch publication and device routing stay in one place. The catalog
        // never includes AgentToolCall, authentication or privilege grants.
        let reply = Box::pin(self.handle(&method, args.params)).await?;
        match reply {
            RpcReply::Value(value) => Ok(value),
            // Desktop watches become one-shot snapshots; dropping the stream
            // immediately releases its subscription.
            RpcReply::Stream(mut stream) => {
                tokio::time::timeout(std::time::Duration::from_secs(15), stream.next())
                    .await
                    .map_err(|_| RpcError::Failed("Management snapshot timed out".into()))?
                    .ok_or_else(|| RpcError::Failed("Management snapshot ended".into()))
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sidebar_and_session_guide_examples_are_valid_mutations() {
        let catalog = mast_management::operations();
        for example in catalog["Mutate"]["examples"].as_array().unwrap() {
            serde_json::from_value::<MutateParams>(example.clone()).unwrap();
        }
    }
}
