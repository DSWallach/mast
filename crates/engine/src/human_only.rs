//! Route-level gate for methods only the Mast app itself may call.
//!
//! The engine serves one `RpcService` on three kinds of transport: the app's
//! in-process connection, the localhost WebSocket port (the port agents and
//! other local processes are given), and device-relay connections. Methods in
//! [`mast_rpc::HUMAN_ONLY_METHODS`] (posting a GitHub review) must come from the
//! first only. Rather than listing what agents may not call, the two outward
//! transports are wrapped in [`HumanOnlyGate`] when they are built, so a method
//! added to that set is refused there with no per-tool bookkeeping.
use std::sync::Arc;

use async_trait::async_trait;
use mast_rpc::{HUMAN_ONLY_METHODS, RpcError, RpcReply, RpcService};

pub struct HumanOnlyGate {
    inner: Arc<dyn RpcService>,
    transport: &'static str,
}

impl HumanOnlyGate {
    pub fn new(inner: Arc<dyn RpcService>, transport: &'static str) -> Arc<dyn RpcService> {
        Arc::new(Self { inner, transport })
    }
}

#[async_trait]
impl RpcService for HumanOnlyGate {
    async fn handle(&self, method: &str, params: serde_json::Value) -> Result<RpcReply, RpcError> {
        if HUMAN_ONLY_METHODS.contains(&method) {
            tracing::warn!(method, transport = self.transport, "refused a human-only method");
            return Err(RpcError::Failed(format!(
                "{method} is only available from the Mast app itself, not over the {} connection",
                self.transport
            )));
        }
        self.inner.handle(method, params).await
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use mast_rpc::methods;
    use std::sync::Mutex;

    struct Recorder(Mutex<Vec<String>>);

    #[async_trait]
    impl RpcService for Recorder {
        async fn handle(&self, method: &str, _: serde_json::Value) -> Result<RpcReply, RpcError> {
            self.0.lock().unwrap().push(method.to_owned());
            RpcReply::value(&serde_json::json!({"handled": method}))
        }
    }

    async fn call(client: &mast_rpc::RpcClient, method: &str) -> Result<serde_json::Value, RpcError> {
        client.call(method, serde_json::json!({})).await
    }

    #[tokio::test]
    async fn local_port_and_relay_refuse_review_posting_but_the_app_connection_allows_it() {
        let inner = Arc::new(Recorder(Mutex::new(Vec::new())));

        // The localhost WebSocket port agents get (what `serve_ipc` serves).
        let listener = tokio::net::TcpListener::bind(("127.0.0.1", 0)).await.unwrap();
        let port = listener.local_addr().unwrap().port();
        tokio::spawn(mast_rpc::serve_ws_listener(listener, crate::outward_transport(inner.clone(), "local IPC")));
        let agent = mast_rpc::connect_ws(&format!("ws://127.0.0.1:{port}")).await.unwrap();
        let error = agent.call(methods::SET_FRICTION_READ_GRANT, serde_json::json!({"spaceId":"space", "enabled":true})).await.unwrap_err().to_string();
        assert!(error.contains("only available from the Mast app itself"), "{error}");
        for &method in HUMAN_ONLY_METHODS {
            let error = call(&agent, method).await.unwrap_err().to_string();
            assert!(error.contains("only available from the Mast app itself"), "{method}: {error}");
        }

        // A relay client reaches the host through the same wrapper (`start_host_relay`).
        let relay = mast_rpc::memory_client(crate::outward_transport(inner.clone(), "relay"));
        let error = call(&relay, methods::SUBMIT_REVIEW).await.unwrap_err().to_string();
        assert!(error.contains("not over the relay connection"), "{error}");

        // Everything else still works on those transports.
        assert!(call(&agent, methods::GET_REVIEW_DRAFTS).await.is_ok());
        assert!(call(&relay, methods::GET_POSTED_REVIEWS).await.is_ok());
        assert_eq!(inner.0.lock().unwrap().as_slice(), [methods::GET_REVIEW_DRAFTS, methods::GET_POSTED_REVIEWS]);

        // The app's own in-process connection is the raw service.
        let app = mast_rpc::memory_client(inner.clone());
        assert!(call(&app, methods::SUBMIT_REVIEW).await.is_ok());
        assert!(call(&app, methods::PREPARE_REVIEW_SUBMIT).await.is_ok());
        assert!(inner.0.lock().unwrap().iter().any(|m| m == methods::SUBMIT_REVIEW));
    }

    #[test]
    fn every_outward_transport_is_built_through_the_gate() {
        let lib = include_str!("lib.rs");
        let serve_ipc = lib.split_once("pub async fn serve_ipc(").unwrap().1.split_once("\n}\n").unwrap().0;
        assert!(serve_ipc.contains("outward_transport(service"), "serve_ipc must wrap its service");
        let relay = lib.split_once("pub fn start_host_relay(").unwrap().1.split_once("\n    }\n").unwrap().0;
        assert!(relay.contains("outward_transport("), "start_host_relay must wrap its service");
        // The app's in-process engine handle is the only place the raw service is used directly.
        let handle = include_str!("../../ui/src/state/engine_handle.rs");
        assert!(handle.contains("memory_client(service.clone())"));
    }
}
