//! The personal assistant's desktop management bridge. Never accepts arbitrary
//! RPC names: the shared, discoverable catalog is the authority for dispatch.
use super::*;
use mast_proto::{agent_tools::AgentToolCallParams, mast_management};

mod management_view;

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct ManagementCall {
    method: Option<String>,
    #[serde(default = "empty_params")]
    params: serde_json::Value,
    /// Opt-in projection of the reply; see [`management_view`].
    #[serde(default)]
    view: Option<management_view::ManagementView>,
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
        refuse_assistant_task_spec_write(&method, &args.params)?;
        self.refuse_manager_loosening_itself(&origin.chat_id, &method, &args.params)?;
        // Reuse the desktop handlers: revision checks, validation, persistence,
        // watch publication and device routing stay in one place. The catalog
        // never includes AgentToolCall, authentication or privilege grants.
        let reply = Box::pin(self.handle(&method, args.params)).await?;
        let view = args.view.unwrap_or_default();
        let value = match reply {
            RpcReply::Value(value) => value,
            // Desktop watches become one-shot snapshots; dropping the stream
            // immediately releases its subscription.
            RpcReply::Stream(mut stream) => {
                tokio::time::timeout(std::time::Duration::from_secs(15), stream.next())
                    .await
                    .map_err(|_| RpcError::Failed("Management snapshot timed out".into()))?
                    .ok_or_else(|| RpcError::Failed("Management snapshot ended".into()))?
            }
        };
        Ok(view.apply(value))
    }
}

impl EngineRpc {
    /// `mast_manage` acts as the user, so it is the one way an agent's call
    /// reaches `setManagerReadOnly`. A chat that is also a project's Manager
    /// (the personal assistant given a Manager role) must not use it to lift
    /// its own read-only runs: only the user, in the app, does that. Turning
    /// the setting on, and changing another project's, stay allowed.
    fn refuse_manager_loosening_itself(
        &self,
        chat_id: &str,
        method: &str,
        params: &serde_json::Value,
    ) -> Result<(), RpcError> {
        if method != "WorkOutlineMutate" {
            return Ok(());
        }
        // The typed mutation, as the handler will parse it, so no other shape
        // of the same call slips past a check on raw keys.
        let Ok(mast_proto::WorkOutlineMutation::SetManagerReadOnly(change)) =
            serde_json::from_value::<mast_proto::WorkOutlineMutation>(params.clone())
        else {
            return Ok(());
        };
        if change.read_only {
            return Ok(());
        }
        if self
            .workspace
            .manager_scope_item_of_chat(chat_id)
            .as_deref()
            == Some(change.work_item_id.as_str())
        {
            return Err(RpcError::Failed(
                "unauthorized: a Manager cannot turn off its own read-only runs; ask the user to change it in the project's settings"
                    .into(),
            ));
        }
        Ok(())
    }
}

/// The assistant delegates ordinary Work Outline updates to desktop handlers,
/// but structured task specifications require a direct user-side call.
fn refuse_assistant_task_spec_write(method: &str, params: &serde_json::Value) -> Result<(), RpcError> {
    if method == methods::WORK_OUTLINE_MUTATE
        && matches!(
            serde_json::from_value::<mast_proto::WorkOutlineMutation>(params.clone()),
            Ok(mast_proto::WorkOutlineMutation::UpdateWorkItem(change))
                if !change.changes.task_spec.is_unchanged()
        )
    {
        return Err(RpcError::Failed(
            "unauthorized: task specifications require a direct desktop update".into(),
        ));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn catalog_task_spec_cannot_be_set_or_cleared_through_assistant_management() {
        for spec in [serde_json::json!({"requiredReport": true}), serde_json::Value::Null] {
            let params = serde_json::json!({
                "op": "updateWorkItem", "workItemId": "item", "operationId": "op",
                "changes": {"taskSpec": spec},
            });
            assert!(refuse_assistant_task_spec_write(methods::WORK_OUTLINE_MUTATE, &params).is_err());
        }
        let params = serde_json::json!({
            "op": "updateWorkItem", "workItemId": "item", "operationId": "op",
            "changes": {"title": "Renamed"},
        });
        assert!(refuse_assistant_task_spec_write(methods::WORK_OUTLINE_MUTATE, &params).is_ok());
    }

    #[test]
    fn sidebar_and_session_guide_examples_are_valid_mutations() {
        let catalog = mast_management::operations();
        for example in catalog["Mutate"]["examples"].as_array().unwrap() {
            serde_json::from_value::<MutateParams>(example.clone()).unwrap();
        }
    }
}
