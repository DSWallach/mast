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
