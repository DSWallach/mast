use super::*;
use std::collections::{BTreeMap, BTreeSet};

#[test]
fn every_rpc_method_has_a_route_and_consistent_forwarding_policy() {
    let declarations = include_str!("../../../rpc/src/lib.rs")
        .split_once("pub mod methods {").unwrap().1
        .split_once("\n}").unwrap().0;
    let methods: BTreeMap<_, _> = declarations.lines().filter_map(|line| {
        let declaration = line.trim().strip_prefix("pub const ")?;
        let (name, value) = declaration.split_once(": &str = \"")?;
        Some((name, value.split_once('"').unwrap().0))
    }).collect();
    let source = include_str!("../rpc.rs");
    let engine = source.split_once("impl RpcService for EngineRpc {").unwrap().1;
    let auth = source.split_once("impl RpcService for AuthRpc {").unwrap().1
        .split_once("impl RpcService for EngineRpc {").unwrap().0;
    let arm_names = |body: &str| -> BTreeSet<String> {
        body.lines().filter_map(|line| {
            let line = line.strip_prefix("            methods::")?;
            let pattern = line.split_once("=>")?.0;
            Some(pattern.split('|').map(|part| {
                part.trim().strip_prefix("methods::").unwrap_or(part.trim()).to_string()
            }).collect::<Vec<_>>())
        }).flatten().collect()
    };
    let routed = arm_names(engine);
    let auth_routed = arm_names(auth);
    let unsupported = ["STOP_ENGINE"]; // Intercepted by the outer engine service.
    for (&name, &value) in &methods {
        let delegated = AuthRpc::handles(value) && auth_routed.contains(name);
        assert!(routed.contains(name) || delegated || unsupported.contains(&name), "{name} has no route");
        assert!(!is_stream_method(value) || forwardable(value), "{name} streams without forwarding");
    }
    for name in routed.iter().chain(auth_routed.iter()) {
        assert!(methods.contains_key(name.as_str()), "route {name} is not a declared method");
    }
    for &name in &unsupported {
        assert!(methods.contains_key(name));
        assert!(!routed.contains(name) && !auth_routed.contains(name));
    }
}

/// Only a human click may post a review. The post path must not be
/// device-forwardable, and no agent-facing surface may name it or call into it.
#[test]
fn agent_surfaces_cannot_reach_the_review_post_path() {
    for method in [methods::SUBMIT_REVIEW, methods::GET_POSTED_REVIEWS] {
        assert!(!forwardable(method), "{method} must not be forwardable");
    }
    // `submit_review` / `SUBMIT_REVIEW` / the wire type may appear only in the
    // human RPC route, the pr_dash post path, and the proto/rpc declarations.
    let agent_surfaces: [(&str, &str); 7] = [
        ("engine agent_tools.rs", include_str!("../agent_tools.rs")),
        ("agent_tools/admin_tool.rs", include_str!("../agent_tools/admin_tool.rs")),
        ("agent_tools/archive.rs", include_str!("../agent_tools/archive.rs")),
        ("agent_tools/health.rs", include_str!("../agent_tools/health.rs")),
        ("agent_tools/workflow_pack_tool.rs", include_str!("../agent_tools/workflow_pack_tool.rs")),
        ("mcp mast.rs", include_str!("../../../mcp/src/mast.rs")),
        ("mcp tool_catalog.rs", include_str!("../../../mcp/src/tools/tool_catalog.rs")),
    ];
    let mut surfaces = agent_surfaces.to_vec();
    surfaces.push(("proto agent.rs", include_str!("../../../proto/src/agent.rs")));
    for (name, text) in surfaces {
        for needle in ["submit_review", "SUBMIT_REVIEW", "SubmitReview", "SubmitReviewParams", "\"SubmitReview\"", "pulls/", "/reviews"] {
            assert!(!text.contains(needle), "{name} must not reference the review post path ({needle})");
        }
    }
    // The only caller of PrDash::submit_review in the engine is the RPC arm.
    let rpc = include_str!("../rpc.rs");
    assert_eq!(rpc.matches(".submit_review(").count(), 1);
    assert!(rpc.contains("methods::SUBMIT_REVIEW =>"));
}
