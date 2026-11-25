use super::*;

#[test]
fn acp_permissions_read_gemini_mcp_metadata_and_commands() {
    use mast_proto::PermissionRequest;
    let gemini_mcp = json!({"kind":"other", "title":"notion: notion-update-page",
        "_meta":{"gemini-cli":{"mcp":{"server_name":"notion", "tool_name":"notion-update-page"}}}});
    assert_eq!(
        acp_permission(&gemini_mcp, true),
        Some(PermissionRequest::McpTool {
            server: "notion".into(),
            tool: "notion-update-page".into()
        })
    );
    let raw = json!({"kind":"execute", "title":"Run tests", "rawInput":{"command":"cargo test"}});
    assert_eq!(
        acp_permission(&raw, false),
        Some(PermissionRequest::Command {
            command: json!("cargo test")
        })
    );
    // Only an agent that puts the exact command in the title is trusted for it.
    let titled = json!({"kind":"execute", "title":"git log -3"});
    assert_eq!(acp_permission(&titled, false), None);
    assert_eq!(
        acp_permission(&titled, true),
        Some(PermissionRequest::Command {
            command: json!("git log -3")
        })
    );
    assert_eq!(
        acp_permission(&json!({"kind":"edit", "title":"x"}), true),
        None
    );
}

#[test]
fn rules_mode_keeps_the_agent_asking() {
    let response = json!({"configOptions":[{"id":"mode", "type":"select", "category":"mode",
        "options":[{"value":"default"}, {"value":"yolo"}]}]});
    let no_opts = serde_json::Map::new();
    assert_eq!(
        config_option_sets(&response, None, &[], &no_opts, false),
        vec![("mode".to_owned(), json!({"value":"yolo"}))]
    );
    assert!(config_option_sets(&response, None, &[], &no_opts, true).is_empty());
}

#[test]
fn pi_discovery_allows_cold_extension_startup() {
    let pi = AcpHarness::pi();
    assert_eq!(pi.model_discovery_timeout, Duration::from_secs(60));
    assert_eq!(pi.handshake_timeout, Duration::from_secs(120));
    assert!(pi.spec.prompt_stall.is_none());
}

#[test]
fn antigravity_discovery_budget_covers_cold_start_without_changing_handshake() {
    let harness = AcpHarness::antigravity();
    assert_eq!(harness.model_discovery_timeout, Duration::from_secs(90));
    assert_eq!(harness.handshake_timeout, Duration::from_secs(120));
    assert_eq!(
        AcpHarness::grok().model_discovery_timeout,
        Duration::from_secs(10)
    );
}

fn all_antigravity_auth_methods() -> Value {
    json!({
        "authMethods": [
            {"id": "oauth-personal"},
            {"id": "oauth-business"},
            {"id": "agent-platform"},
            {"id": "gemini-api-key"}
        ]
    })
}

fn settings_holding(body: &str) -> (tempfile::TempDir, PathBuf) {
    let home = tempfile::tempdir().unwrap();
    let dir = home.path().join("antigravity-acp");
    std::fs::create_dir_all(&dir).unwrap();
    let path = dir.join("settings.json");
    std::fs::write(&path, body).unwrap();
    (home, path)
}

#[test]
fn antigravity_sign_in_preserves_an_advertised_configured_method() {
    let initialized = all_antigravity_auth_methods();
    assert_eq!(
        sign_in_auth_method(
            &initialized,
            "oauth-personal",
            Some(&ConfiguredAuthMethod::new("oauth-business".into()))
        )
        .unwrap(),
        "oauth-business"
    );
    assert_eq!(
        sign_in_auth_method(&initialized, "oauth-personal", None).unwrap(),
        "oauth-personal"
    );
}

#[test]
fn antigravity_sign_in_refuses_to_replace_an_unavailable_configured_method() {
    let initialized = json!({
        "authMethods": [{"id": "oauth-personal"}]
    });
    let error = sign_in_auth_method(
        &initialized,
        "oauth-personal",
        Some(&ConfiguredAuthMethod::new("oauth-business".into())),
    )
    .unwrap_err();
    assert!(
        error
            .to_string()
            .contains("configured auth method oauth-business")
    );
}

#[test]
fn antigravity_sign_in_resolves_the_vertex_ai_alias() {
    assert_eq!(
        sign_in_auth_method(
            &all_antigravity_auth_methods(),
            "oauth-personal",
            Some(&ConfiguredAuthMethod::new("vertex-ai".into()))
        )
        .unwrap(),
        "agent-platform"
    );
}

#[test]
fn antigravity_sign_in_still_refuses_an_unknown_configured_method() {
    let error = sign_in_auth_method(
        &all_antigravity_auth_methods(),
        "oauth-personal",
        Some(&ConfiguredAuthMethod::new("totally-made-up".into())),
    )
    .unwrap_err();
    assert!(
        error
            .to_string()
            .contains("configured auth method totally-made-up")
    );
}

#[test]
fn antigravity_settings_reader_accepts_plain_json() {
    let (_home, path) = settings_holding(r#"{"auth": {"type": "oauth-business"}}"#);
    assert_eq!(
        configured_auth_method_in(&path)
            .unwrap()
            .unwrap()
            .configured,
        "oauth-business"
    );
}

#[test]
fn antigravity_settings_reader_accepts_hjson() {
    let (_home, path) = settings_holding(
        "{\n  // the account this machine signs in with\n  auth: {\n    type: gemini-api-key\n  }\n}\n",
    );
    let method = configured_auth_method_in(&path).unwrap().unwrap();
    assert_eq!(method.configured, "gemini-api-key");
    assert_eq!(method.canonical, "gemini-api-key");
}

#[test]
fn antigravity_settings_reader_canonicalizes_vertex_ai() {
    let (_home, path) = settings_holding(r#"{"auth": {"type": "vertex-ai"}}"#);
    let method = configured_auth_method_in(&path).unwrap().unwrap();
    assert_eq!(method.configured, "vertex-ai");
    assert_eq!(method.canonical, "agent-platform");
}

#[test]
fn antigravity_settings_reader_reports_no_method_when_unset() {
    let (_home, empty) = settings_holding("{}");
    assert!(configured_auth_method_in(&empty).unwrap().is_none());

    let (_home, blank) = settings_holding(r#"{"auth": {"type": ""}}"#);
    assert!(configured_auth_method_in(&blank).unwrap().is_none());

    let (missing, _) = settings_holding("{}");
    let absent = missing.path().join("nowhere").join("settings.json");
    assert!(configured_auth_method_in(&absent).unwrap().is_none());
}

#[test]
fn antigravity_home_expands_a_home_relative_gemini_home() {
    let home = tempfile::tempdir().unwrap();
    let expanded =
        antigravity_paths::expand_user(Path::new("~/gemini-home"), Some(home.path())).unwrap();
    assert_eq!(expanded, home.path().join("gemini-home"));

    assert_eq!(
        antigravity_paths::expand_user(Path::new("~"), Some(home.path())).unwrap(),
        home.path()
    );
    assert_eq!(
        antigravity_paths::expand_user(Path::new("/absolute/gemini"), Some(home.path())).unwrap(),
        Path::new("/absolute/gemini")
    );
    assert!(antigravity_paths::expand_user(Path::new("~/gemini"), None).is_err());
}

#[test]
fn antigravity_home_relative_settings_still_resolve_the_configured_method() {
    let home = tempfile::tempdir().unwrap();
    let gemini_home =
        antigravity_paths::expand_user(Path::new("~/.gemini"), Some(home.path())).unwrap();
    let dir = gemini_home.join("antigravity-acp");
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(
        dir.join("settings.json"),
        r#"{"auth": {"type": "oauth-business"}}"#,
    )
    .unwrap();

    let method = configured_auth_method_in(&dir.join("settings.json"))
        .unwrap()
        .unwrap();
    assert_eq!(method.canonical, "oauth-business");
}

fn selected_auth_in_home(gemini_home: &Path) -> String {
    let configured =
        configured_auth_method_in(&gemini_home.join("antigravity-acp").join("settings.json"))
            .unwrap();
    sign_in_auth_method(
        &all_antigravity_auth_methods(),
        "oauth-personal",
        configured.as_ref(),
    )
    .unwrap()
}

fn write_business_auth(gemini_home: &Path) {
    let settings = gemini_home.join("antigravity-acp");
    std::fs::create_dir_all(&settings).unwrap();
    std::fs::write(
        settings.join("settings.json"),
        r#"{"auth":{"type":"oauth-business"}}"#,
    )
    .unwrap();
}

#[cfg(unix)]
#[test]
fn antigravity_named_home_resolves_from_passwd() {
    let (username, real_home) = antigravity_paths::passwd_entry(None).unwrap();
    let path = PathBuf::from(format!("~{username}")).join("fixture");
    let unrelated_home = tempfile::tempdir().unwrap();
    let resolved = antigravity_paths::resolve_home(
        Some(&path),
        Some(unrelated_home.path()),
        unrelated_home.path(),
    )
    .unwrap();
    assert_eq!(resolved, real_home.join("fixture"));
}

#[test]
fn antigravity_relative_home_settings_use_the_child_working_directory() {
    let parent_cwd = tempfile::tempdir().unwrap();
    let child_cwd = tempfile::tempdir().unwrap();
    let relative = Path::new("relative-gemini-home");
    write_business_auth(&child_cwd.path().join(relative));
    assert!(!parent_cwd.path().join(relative).exists());
    let resolved =
        antigravity_paths::resolve_home(Some(relative), Some(child_cwd.path()), child_cwd.path())
            .unwrap();
    assert_eq!(resolved, child_cwd.path().join(relative));
    assert_eq!(selected_auth_in_home(&resolved), "oauth-business");
    let command_home = parent_cwd.path().join(&resolved);
    assert_eq!(selected_auth_in_home(&command_home), "oauth-business");
}

#[test]
fn antigravity_empty_home_fails_before_auth_selection() {
    let child_cwd = tempfile::tempdir().unwrap();
    assert!(
        antigravity_paths::resolve_home(
            Some(Path::new("")),
            Some(child_cwd.path()),
            child_cwd.path(),
        )
        .is_err()
    );
}

#[cfg(unix)]
#[test]
fn antigravity_unknown_named_home_fails_before_auth_selection() {
    let cwd = tempfile::tempdir().unwrap();
    let path = PathBuf::from(format!("~zeron-missing-{}", uuid::Uuid::new_v4()));
    assert!(antigravity_paths::resolve_home(Some(&path), Some(cwd.path()), cwd.path()).is_err());
}

#[test]
fn antigravity_sign_in_accepts_the_configured_method_url() {
    assert_eq!(
        sign_in_url("Continue at https://business.example.test/login?id=7."),
        Some("https://business.example.test/login?id=7".into())
    );
    assert_eq!(
        sign_in_url("Open http://127.0.0.1:8080/callback"),
        Some("http://127.0.0.1:8080/callback".into())
    );
}

#[test]
fn devin_initialize_enables_only_the_supported_subagent_stream() {
    let devin = initialize_params(HarnessId::Devin);
    let meta = &devin["clientCapabilities"]["_meta"];
    assert_eq!(meta["cognition.ai/subagentSupport"], true);
    assert!(meta.get("cognition.ai/subagentControl").is_none());

    let grok = initialize_params(HarnessId::Grok);
    assert!(grok["clientCapabilities"].get("_meta").is_none());
}

#[test]
fn steering_capability_reads_initialize_meta() {
    assert!(steering_supported(&json!({
        "protocolVersion": 1,
        "_meta": { "steering": { "supported": true } },
    })));
    assert!(!steering_supported(&json!({ "protocolVersion": 1 })));
    assert!(!steering_supported(&json!({
        "_meta": { "steering": { "supported": false } },
    })));
}

#[test]
fn config_option_sets_map_model_effort_and_model_options() {
    let response = json!({
        "sessionId": "s-1",
        "configOptions": [
            {
                "id": "model",
                "name": "Model",
                "category": "model",
                "type": "select",
                "currentValue": "claude-sonnet-5",
                "options": [
                    { "value": "claude-sonnet-5", "name": "Sonnet 5" },
                    { "value": "claude-opus-5", "name": "Opus 5" },
                    { "value": "claude-opus-5[1m]", "name": "Opus 5 (1M)" },
                ],
            },
            {
                "id": "effort",
                "name": "Reasoning effort",
                "category": "thought_level",
                "type": "select",
                "currentValue": "high",
                "options": [
                    { "value": "low", "name": "Low" },
                    { "value": "medium", "name": "Medium" },
                    { "value": "high", "name": "High" },
                    { "value": "max", "name": "Max" },
                ],
            },
            {
                "id": "fast_mode",
                "name": "Fast mode",
                "category": "model_config",
                "type": "boolean",
                "currentValue": false,
            },
        ],
    });
    let no_opts = serde_json::Map::new();
    // Model switch + effort preference list; fastMode untouched without a
    // model-option selection.
    assert_eq!(
        config_option_sets(
            &response,
            Some("claude-opus-5"),
            &["medium"],
            &no_opts,
            false
        ),
        vec![
            ("model".to_owned(), json!({ "value": "claude-opus-5" })),
            ("effort".to_owned(), json!({ "value": "medium" })),
        ]
    );
    // Effort preference order: first ADVERTISED candidate wins.
    assert_eq!(
        config_option_sets(&response, None, &["xhigh", "max"], &no_opts, false),
        vec![("effort".to_owned(), json!({ "value": "max" }))]
    );
    // contextWindow=1m composes the [1m] model id; fastMode=on matches the
    // boolean option across naming styles (fastMode vs fast_mode).
    let mut opts = serde_json::Map::new();
    opts.insert("contextWindow".into(), json!("1m"));
    opts.insert("fastMode".into(), json!("on"));
    assert_eq!(
        config_option_sets(&response, Some("claude-opus-5"), &["high"], &opts, false),
        vec![
            ("model".to_owned(), json!({ "value": "claude-opus-5[1m]" })),
            (
                "fast_mode".to_owned(),
                json!({ "type": "boolean", "value": true })
            ),
        ]
    );
    // Already-current values and unadvertised models set nothing.
    assert_eq!(
        config_option_sets(
            &response,
            Some("claude-sonnet-5"),
            &["high"],
            &no_opts,
            false
        ),
        Vec::new()
    );
    assert_eq!(
        config_option_sets(&response, Some("gpt-5.6-sol"), &[], &no_opts, false),
        Vec::new()
    );
    // No configOptions advertised → nothing to set.
    assert_eq!(
        config_option_sets(
            &json!({"sessionId": "s"}),
            Some("x"),
            &["high"],
            &no_opts,
            false
        ),
        Vec::new()
    );
}

#[test]
fn first_class_models_use_session_set_model_without_config_option() {
    let response = json!({
        "models": {
            "currentModelId": "grok-4.6",
            "availableModels": [
                { "modelId": "grok-4.6", "name": "Grok 4.6" },
                { "modelId": "grok-4.5", "name": "Grok 4.5" },
            ],
        },
    });
    assert_eq!(
        first_class_model_change(&response, Some("grok-4.5")).unwrap(),
        Some("grok-4.5".into())
    );
    assert_eq!(
        first_class_model_change(&response, Some("grok-4.6")).unwrap(),
        None
    );
    assert!(first_class_model_change(&response, Some("unknown")).is_err());
}

#[test]
fn model_config_option_takes_precedence_over_legacy_models_state() {
    let response = json!({
        "models": {
            "currentModelId": "gpt-5.6-sol low",
            "availableModels": [
                { "modelId": "gpt-5.6-sol low", "name": "GPT-5.6-Sol (low)" },
            ],
        },
        "configOptions": [{
            "id": "model",
            "category": "model",
            "type": "select",
            "currentValue": "gpt-5.6-sol",
            "options": [
                { "value": "gpt-5.6-sol", "name": "GPT-5.6-Sol" },
                { "value": "gpt-5.6-terra", "name": "GPT-5.6-Terra" },
            ],
        }],
    });
    assert_eq!(
        first_class_model_change(&response, Some("gpt-5.6-terra")).unwrap(),
        None
    );
}

#[test]
fn models_prefer_the_model_config_option_over_legacy_available_models() {
    // codex-acp shape: the legacy models state enumerates model × effort,
    // the config options carry base ids + a separate thought_level select.
    let response = json!({
        "sessionId": "s-1",
        "models": {
            "currentModelId": "gpt-5.6-sol low",
            "availableModels": [
                { "modelId": "gpt-5.6-sol low", "name": "GPT-5.6-Sol (low)" },
                { "modelId": "gpt-5.6-sol medium", "name": "GPT-5.6-Sol (medium)" },
                { "modelId": "gpt-5.6-terra low", "name": "GPT-5.6-Terra (low)" },
            ],
        },
        "configOptions": [
            {
                "id": "mode",
                "name": "Mode",
                "category": "mode",
                "type": "select",
                "currentValue": "agent",
                "options": [
                    { "value": "read-only", "name": "Read Only" },
                    { "value": "agent", "name": "Agent" },
                    { "value": "agent-full-access", "name": "Agent (full access)" },
                ],
            },
            {
                "id": "model",
                "name": "Model",
                "category": "model",
                "type": "select",
                "currentValue": "gpt-5.6-sol",
                "options": [
                    { "value": "gpt-5.6-sol", "name": "GPT-5.6-Sol", "description": "Frontier" },
                    { "value": "gpt-5.6-terra", "name": "GPT-5.6-Terra" },
                ],
            },
            {
                "id": "reasoning_effort",
                "name": "Reasoning effort",
                "category": "thought_level",
                "type": "select",
                "currentValue": "medium",
                "options": [
                    { "value": "low", "name": "Low" },
                    { "value": "medium", "name": "Medium" },
                    { "value": "high", "name": "High" },
                ],
            },
            {
                "id": "fast-mode",
                "name": "Fast mode",
                "category": "model_config",
                "type": "select",
                "currentValue": "off",
                "options": [
                    { "value": "off", "name": "Off" },
                    { "value": "on", "name": "On" },
                ],
            },
        ],
    });
    let models = models_from_session(&response, &crate::codex::catalog::static_models());
    // Two base models — never one row per effort variant.
    assert_eq!(
        models.iter().map(|m| m.id.as_str()).collect::<Vec<_>>(),
        vec!["gpt-5.6-sol", "gpt-5.6-terra"]
    );
    // Catalog match keeps the curated per-model ladder; wire wins on
    // label/description.
    assert_eq!(models[0].label, "GPT-5.6-Sol");
    assert_eq!(models[0].description.as_deref(), Some("Frontier"));
    assert!(models[0].reasoning_levels.contains(&ReasoningLevel::Ultra));
    // Wire config options become traits; mode/model/thought_level do not.
    assert_eq!(
        models[0]
            .options
            .iter()
            .map(|o| o.id.as_str())
            .collect::<Vec<_>>(),
        vec!["fast-mode"]
    );
    assert_eq!(models[0].options[0].default_choice, "off");
}

#[test]
fn model_1m_variants_collapse_into_a_context_window_trait() {
    let response = json!({
        "sessionId": "s-1",
        "configOptions": [
            {
                "id": "model",
                "category": "model",
                "type": "select",
                "currentValue": "claude-sonnet-5",
                "options": [
                    { "value": "claude-sonnet-5", "name": "Sonnet 5" },
                    { "value": "claude-sonnet-5[1m]", "name": "Sonnet 5 (1M)" },
                    // SDK-id hint spelling collapses too.
                    { "value": "claude-opus-4-6", "name": "Opus 4.6" },
                    { "value": "claude-opus-4-6-1m", "name": "Opus 4.6 (1M)" },
                    { "value": "claude-haiku-4-5", "name": "Haiku 4.5" },
                ],
            },
        ],
    });
    let models = models_from_session(&response, &[]);
    assert_eq!(
        models.iter().map(|m| m.id.as_str()).collect::<Vec<_>>(),
        vec!["claude-sonnet-5", "claude-opus-4-6", "claude-haiku-4-5"]
    );
    assert!(models[0].options.iter().any(|o| o.id == "contextWindow"));
    assert!(models[1].options.iter().any(|o| o.id == "contextWindow"));
    assert!(models[2].options.is_empty());
}

#[test]
fn default_alias_drops_and_orphan_1m_variants_fold_to_their_base() {
    // The real claude adapter advertises a `default` alias row plus
    // `opus[1m]` with NO bare `opus` (the CLI pins the 1M window).
    // Both made the picker read like a settings dump (user report):
    // `default` duplicates a real model, and the orphan 1M variant now
    // presents AS its base model with the Context Window trait pinned
    // to 1M.
    let response = json!({
        "sessionId": "s-1",
        "configOptions": [{
            "id": "model",
            "category": "model",
            "type": "select",
            "currentValue": "claude-fable-5[1m]",
            "options": [
                { "value": "default", "name": "Default (recommended)" },
                { "value": "opus[1m]", "name": "Opus (1M context)" },
                { "value": "claude-fable-5[1m]", "name": "Fable 5" },
                { "value": "sonnet", "name": "Sonnet" },
                { "value": "haiku", "name": "Haiku" },
            ],
        }],
    });
    let models = models_from_session(&response, &[]);
    assert_eq!(
        models.iter().map(|m| m.id.as_str()).collect::<Vec<_>>(),
        vec!["opus", "claude-fable-5", "sonnet", "haiku"]
    );
    // The folded rows keep a de-parenthesized wire name (no catalog
    // here) and carry the 1M-pinned window trait.
    assert_eq!(models[0].label, "Opus");
    let window = models[0].options.iter().find(|o| o.id == "contextWindow");
    assert_eq!(window.map(|o| o.default_choice.as_str()), Some("1m"));
    assert!(
        models[1]
            .options
            .iter()
            .any(|o| o.id == "contextWindow" && o.default_choice == "1m")
    );
    // The bare aliases stay untouched.
    assert!(models[2].options.is_empty());
    assert!(models[3].options.is_empty());
}

#[test]
fn claude_aliases_enrich_from_the_curated_catalog() {
    // Same wire shape, WITH the claude catalog: bare aliases pick up the
    // flagship row's curated label/description/ladder, versioned ids
    // keep their wire name.
    let response = json!({
        "sessionId": "s-1",
        "configOptions": [{
            "id": "model",
            "category": "model",
            "type": "select",
            "currentValue": "default",
            "options": [
                { "value": "default", "name": "Default (recommended)" },
                { "value": "opus[1m]", "name": "Opus (1M context)" },
                { "value": "fable", "name": "Fable" },
                { "value": "sonnet", "name": "Sonnet" },
                { "value": "haiku", "name": "Haiku" },
            ],
        }],
    });
    let models = models_from_session(&response, &crate::claude::catalog::static_models());
    assert_eq!(
        models.iter().map(|m| m.label.as_str()).collect::<Vec<_>>(),
        vec!["Opus 5.5", "Fable 5.1", "Sonnet 5", "Haiku 4.5"]
    );
    // The alias rows carry the catalog's per-model ladders.
    assert!(
        models[1]
            .reasoning_levels
            .contains(&ReasoningLevel::Ultracode)
    );
    assert!(models[3].reasoning_levels.is_empty());
    // Versioned ids never fuzzy-match: a foreign id passes through.
    let foreign = json!({
        "sessionId": "s-1",
        "configOptions": [{
            "id": "model", "category": "model", "type": "select",
            "options": [{ "value": "claude-opus-9-mini", "name": "Opus 9 Mini" }],
        }],
    });
    let models = models_from_session(&foreign, &crate::claude::catalog::static_models());
    assert_eq!(models[0].label, "Opus 9 Mini");
}

#[test]
fn models_fall_back_to_legacy_state_with_catalog_options() {
    let response = json!({
        "sessionId": "s-1",
        "models": {
            "availableModels": [
                { "modelId": "gpt-5.6-sol", "name": "GPT-5.6-Sol" },
                { "modelId": "gpt-x", "name": "GPT-X" },
            ],
        },
    });
    let models = models_from_session(&response, &crate::codex::catalog::static_models());
    assert_eq!(models.len(), 2);
    // Catalog-matched id keeps the curated options on the legacy path…
    assert!(models[0].options.iter().any(|o| o.id == "serviceTier"));
    // …unknown ids get none.
    assert!(models[1].options.is_empty());
}

#[test]
fn codex_exec_approval_options_are_not_a_question() {
    // codex-acp's real exec-approval shape: two allow_always entries (the
    // session allow + a prefix-rule amendment). Must auto-accept.
    let options = vec![
        json!({ "optionId": "allow_once", "name": "Allow Once", "kind": "allow_once" }),
        json!({ "optionId": "allow_always", "name": "Allow for Session", "kind": "allow_always" }),
        json!({ "optionId": "allow_prefix", "name": "Allow Commands Starting With `cargo test`", "kind": "allow_always" }),
        json!({ "optionId": "reject", "name": "Reject", "kind": "reject_once" }),
    ];
    assert!(!is_user_question(&options));
    // AskUserQuestion relays choices without allow/reject kinds.
    let question = vec![
        json!({ "optionId": "a", "name": "Blue" }),
        json!({ "optionId": "b", "name": "Green" }),
    ];
    assert!(is_user_question(&question));
    let mixed = vec![
        json!({ "optionId": "a", "name": "Proceed", "kind": "allow_once" }),
        json!({ "optionId": "b", "name": "Другое", "kind": "other" }),
    ];
    assert!(is_user_question(&mixed));
}

#[test]
fn mode_config_option_prefers_a_no_prompt_mode_per_adapter_naming() {
    let codex = json!({
        "sessionId": "s-1",
        "configOptions": [{
            "id": "mode",
            "category": "mode",
            "type": "select",
            "currentValue": "agent",
            "options": [
                { "value": "read-only" },
                { "value": "agent" },
                { "value": "agent-full-access" },
            ],
        }],
    });
    let no_opts = serde_json::Map::new();
    assert_eq!(
        config_option_sets(&codex, None, &[], &no_opts, false),
        vec![("mode".to_owned(), json!({ "value": "agent-full-access" }))]
    );
}

fn antigravity_signed_in_catalog() -> Value {
    json!({
        "configOptions": [{
            "id": "model",
            "category": "model",
            "type": "select",
            "options": [
                {"value": "gemini-3.7-flash-high", "name": "Gemini 3.7 Flash (High)"},
                {"value": "gemini-3.7-flash-medium", "name": "Gemini 3.7 Flash (Medium)"},
                {"value": "gemini-3.7-flash-low", "name": "Gemini 3.7 Flash (Low)"},
                {"value": "gemini-pro-agent", "name": "Gemini 3.1 Pro (High)"},
                {"value": "gemini-3.1-pro-low", "name": "Gemini 3.1 Pro (Low)"},
                {"value": "gemini-legacy", "name": "Gemini Legacy"}
            ]
        }]
    })
}

#[test]
fn effort_variants_group_by_name_even_when_ids_disagree() {
    let models = models_from_session(&antigravity_signed_in_catalog(), &[]);
    let rows: Vec<(String, String, Vec<ReasoningLevel>)> = group_effort_variants(models)
        .into_iter()
        .map(|m| (m.id, m.label, m.reasoning_levels))
        .collect();
    assert_eq!(
        rows,
        vec![
            (
                "gemini-3.7-flash".into(),
                "Gemini 3.7 Flash".into(),
                vec![
                    ReasoningLevel::Low,
                    ReasoningLevel::Medium,
                    ReasoningLevel::High
                ]
            ),
            (
                "gemini-3.1-pro".into(),
                "Gemini 3.1 Pro".into(),
                vec![ReasoningLevel::Low, ReasoningLevel::High]
            ),
            ("gemini-legacy".into(), "Gemini Legacy".into(), vec![]),
        ]
    );
}

/// the pinned 1.1.1 server fetches a signed-in account's flash list
/// dynamically, so a newer flagship than the static catalog knows about can
/// arrive on the wire; it has to reach the picker on its own merits rather
/// than be filtered down to the ids we happen to have curated.
#[test]
fn antigravity_surfaces_a_newer_flash_the_live_catalog_advertises() {
    let live = json!({
        "configOptions": [{
            "id": "model",
            "category": "model",
            "type": "select",
            "options": [
                {"value": "gemini-3.8-flash-high", "name": "Gemini 3.8 Flash (High)"},
                {"value": "gemini-3.8-flash-medium", "name": "Gemini 3.8 Flash (Medium)"},
                {"value": "gemini-3.8-flash-low", "name": "Gemini 3.8 Flash (Low)"},
                {"value": "gemini-3.7-flash-high", "name": "Gemini 3.7 Flash (High)"},
                {"value": "gemini-3.7-flash-medium", "name": "Gemini 3.7 Flash (Medium)"},
                {"value": "gemini-3.7-flash-low", "name": "Gemini 3.7 Flash (Low)"},
                {"value": "gemini-pro-agent", "name": "Gemini 3.1 Pro (High)"},
                {"value": "gemini-3.1-pro-low", "name": "Gemini 3.1 Pro (Low)"}
            ]
        }]
    });
    let models = models_from_session(&live, &(antigravity_spec().models)());
    let rows: Vec<(String, String, Vec<ReasoningLevel>)> = group_effort_variants(models)
        .into_iter()
        .map(|m| (m.id, m.label, m.reasoning_levels))
        .collect();
    assert_eq!(
        rows,
        vec![
            (
                "gemini-3.8-flash".into(),
                "Gemini 3.8 Flash".into(),
                vec![
                    ReasoningLevel::Low,
                    ReasoningLevel::Medium,
                    ReasoningLevel::High
                ]
            ),
            (
                "gemini-3.7-flash".into(),
                "Gemini 3.7 Flash".into(),
                vec![
                    ReasoningLevel::Low,
                    ReasoningLevel::Medium,
                    ReasoningLevel::High
                ]
            ),
            (
                "gemini-3.1-pro".into(),
                "Gemini 3.1 Pro".into(),
                vec![ReasoningLevel::Low, ReasoningLevel::High]
            ),
        ]
    );
    assert_eq!(
        effort_variant_id(&live, "gemini-3.8-flash", Some(ReasoningLevel::Medium)),
        "gemini-3.8-flash-medium"
    );
}

/// the offline list is what an unreachable or signed-out server falls back
/// to, so it stays at what the pinned 1.1.1 archive itself bundles. a newer
/// flash belongs here only once that pin advertises it.
#[test]
fn antigravity_static_fallback_stays_on_the_pinned_servers_models() {
    let fallback: Vec<(String, Vec<ReasoningLevel>)> = (antigravity_spec().models)()
        .into_iter()
        .map(|m| (m.id, m.reasoning_levels))
        .collect();
    assert_eq!(
        fallback,
        vec![
            (
                "gemini-4-argon".into(),
                vec![
                    ReasoningLevel::Low,
                    ReasoningLevel::Medium,
                    ReasoningLevel::High
                ]
            ),
            (
                "gemini-3.8-flash".into(),
                vec![
                    ReasoningLevel::Low,
                    ReasoningLevel::Medium,
                    ReasoningLevel::High
                ]
            ),
            (
                "gemini-3.7-flash".into(),
                vec![
                    ReasoningLevel::Low,
                    ReasoningLevel::Medium,
                    ReasoningLevel::High
                ]
            ),
            (
                "gemini-3.1-pro".into(),
                vec![ReasoningLevel::Low, ReasoningLevel::High]
            ),
        ]
    );
}

#[test]
fn antigravity_cli_1_5_6_base_models_and_thought_level_work_with_effort_grouping() {
    let live = json!({
        "configOptions": [
            {
                "id": "model",
                "name": "Model",
                "category": "model",
                "type": "select",
                "currentValue": "gemini-4-argon",
                "options": [
                    {"value": "gemini-4-argon", "name": "Gemini 4 Argon"},
                    {"value": "gemini-3.8-flash", "name": "Gemini 3.8 Flash"},
                    {"value": "gpt-oss-120b-medium", "name": "GPT-OSS 120B (Medium)"}
                ]
            },
            {
                "id": "thought_level",
                "name": "Thinking Level",
                "category": "thought_level",
                "type": "select",
                "currentValue": "medium",
                "options": [
                    {"value": "low", "name": "Low"},
                    {"value": "medium", "name": "Medium"},
                    {"value": "high", "name": "High"}
                ]
            }
        ]
    });
    let models = group_effort_variants(models_from_session(&live, &(antigravity_spec().models)()));
    let rows: Vec<(String, String, Vec<ReasoningLevel>)> = models
        .into_iter()
        .map(|m| (m.id, m.label, m.reasoning_levels))
        .collect();
    assert_eq!(
        rows,
        vec![
            (
                "gemini-4-argon".into(),
                "Gemini 4 Argon".into(),
                vec![
                    ReasoningLevel::Low,
                    ReasoningLevel::Medium,
                    ReasoningLevel::High
                ]
            ),
            (
                "gemini-3.8-flash".into(),
                "Gemini 3.8 Flash".into(),
                vec![
                    ReasoningLevel::Low,
                    ReasoningLevel::Medium,
                    ReasoningLevel::High
                ]
            ),
            (
                "gpt-oss-120b".into(),
                "GPT-OSS 120B".into(),
                vec![ReasoningLevel::Medium]
            ),
        ]
    );
    // Base model IDs pass through to config_option_sets, which sets both
    // `model` and `thought_level`; saved variant IDs strip to the base ID;
    // single-level suffixed models map back to their advertised variant.
    assert_eq!(
        effort_variant_id(&live, "gemini-3.8-flash", Some(ReasoningLevel::High)),
        "gemini-3.8-flash"
    );
    assert_eq!(
        effort_variant_id(&live, "gemini-3.8-flash-high", Some(ReasoningLevel::High)),
        "gemini-3.8-flash"
    );
    assert_eq!(
        effort_variant_id(&live, "gpt-oss-120b", Some(ReasoningLevel::High)),
        "gpt-oss-120b-medium"
    );
    assert_eq!(
        config_option_sets(
            &live,
            Some("gemini-3.8-flash"),
            &["high"],
            &serde_json::Map::new(),
            false
        ),
        vec![
            ("model".to_owned(), json!({"value": "gemini-3.8-flash"})),
            ("thought_level".to_owned(), json!({"value": "high"})),
        ]
    );
}

#[test]
fn antigravity_session_params_include_mcp_servers_and_system_prompt_append() {
    let mut req: RunRequest = serde_json::from_value(json!({
        "prompt": "hi",
        "model": "gemini-4-argon",
        "reasoning": "high",
        "cwd": "/work/space",
        "sandbox": "workspace-write",
        "resume": null
    }))
    .unwrap();
    let rendered =
        "<zeron_work_outline_instructions>\nFollow outline\n</zeron_work_outline_instructions>\n";
    req.instructions = Some(mast_proto::InstructionDelivery {
        envelope: mast_proto::InstructionEnvelope {
            revision_id: "r1".into(),
            revision_hash: "h1".into(),
            envelope_version: 1,
            envelope_sha256: mast_proto::envelope_sha256(rendered),
            rendered: rendered.into(),
        },
        transition: mast_proto::InstructionTransition::Fresh,
    });
    let tools = crate::AgentToolsInjection {
        server_name: "mast-agent".into(),
        command: PathBuf::from("/usr/local/bin/mast"),
        args: vec!["mcp".into()],
        env: [("MAST_AGENT_TOKEN".to_owned(), "tok-1".to_owned())]
            .into_iter()
            .collect(),
    };
    let params = build_session_params(HarnessId::Antigravity, &req, Some(&tools), None);
    assert_eq!(params["cwd"], "/work/space");
    assert_eq!(
        params["mcpServers"],
        json!([{
            "name": "mast-agent",
            "command": "/usr/local/bin/mast",
            "args": ["mcp"],
            "env": [{"name": "MAST_AGENT_TOKEN", "value": "tok-1"}]
        }])
    );
    assert_eq!(
        params["_meta"]["antigravity"]["systemPrompt"]["append"],
        format!("{rendered}{ANTIGRAVITY_BACKGROUND_TASK_GUIDANCE}")
    );
    let with_agent = build_session_params(
        HarnessId::Antigravity,
        &req,
        None,
        Some(&format!("Review.\n\n{rendered}")),
    );
    assert_eq!(
        with_agent["_meta"]["antigravity"]["systemPrompt"]["append"],
        format!("Review.\n\n{rendered}{ANTIGRAVITY_BACKGROUND_TASK_GUIDANCE}")
    );

    req.instructions = None;
    let params_without_instructions = build_session_params(HarnessId::Antigravity, &req, None, None);
    assert_eq!(params_without_instructions["_meta"]["antigravity"]["systemPrompt"]["append"], ANTIGRAVITY_BACKGROUND_TASK_GUIDANCE);
}

#[cfg(unix)]
#[tokio::test]
async fn antigravity_resolves_a_named_agent_from_claude_definitions() {
    let root = tempfile::tempdir().unwrap();
    let repo = root.path().join("repo");
    std::fs::create_dir_all(repo.join(".git")).unwrap();
    let definition = crate::agent_definition::claude_definition_path(&repo, "pr-review");
    std::fs::create_dir_all(definition.parent().unwrap()).unwrap();
    std::fs::write(
        &definition,
        "---\nname: pr-review\nmodel: opus\ntools: Read, Grep\n---\n\nReview carefully.\n",
    )
    .unwrap();
    let harness = AcpHarness::antigravity().with_executable(
        PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("tests/fixtures/fake-antigravity-acp.sh"),
    );
    let mut req: RunRequest = serde_json::from_value(json!({
        "prompt": "hi", "model": "gemini-4-argon", "reasoning": null,
        "cwd": repo.to_string_lossy(), "sandbox": "workspace-write", "resume": null
    }))
    .unwrap();
    assert_eq!(harness.agent_instructions(&req).await.unwrap(), None);
    req.model_options
        .insert(mast_proto::AGENT_OPTION.into(), json!("pr-review"));
    assert_eq!(
        harness.agent_instructions(&req).await.unwrap().as_deref(),
        Some("Review carefully.\n")
    );
    let rendered = "<zeron_work_outline_instructions>\nX\n</zeron_work_outline_instructions>\n";
    req.instructions = Some(mast_proto::InstructionDelivery {
        envelope: mast_proto::InstructionEnvelope {
            revision_id: "r1".into(),
            revision_hash: "h1".into(),
            envelope_version: 1,
            envelope_sha256: mast_proto::envelope_sha256(rendered),
            rendered: rendered.into(),
        },
        transition: mast_proto::InstructionTransition::Fresh,
    });
    assert_eq!(
        harness.agent_instructions(&req).await.unwrap(),
        Some(format!("Review carefully.\n\n{rendered}"))
    );
    req.model_options
        .insert(mast_proto::AGENT_OPTION.into(), json!("missing"));
    let error = harness.agent_instructions(&req).await.unwrap_err();
    assert!(error.to_string().contains("missing.md"), "{error}");
    assert!(!error.to_string().contains(".toml"), "{error}");
}

#[test]
fn other_acp_agents_refuse_a_named_agent() {
    let mut req: RunRequest = serde_json::from_value(json!({
        "prompt": "hi", "model": null, "reasoning": null,
        "cwd": "/w", "sandbox": "workspace-write", "resume": null
    }))
    .unwrap();
    req.model_options
        .insert(mast_proto::AGENT_OPTION.into(), json!("pr-review"));
    let hermes = AcpHarness::hermes();
    assert!(matches!(
        crate::require_named_agent_support(hermes.id(), hermes.capabilities(), &req),
        Err(HarnessError::Agent { .. })
    ));
}


#[test]
fn effort_variant_id_resolves_the_advertised_id_for_each_level() {
    let catalog = antigravity_signed_in_catalog();
    let id = |model, reasoning| effort_variant_id(&catalog, model, reasoning);
    assert_eq!(
        id("gemini-3.1-pro", Some(ReasoningLevel::High)),
        "gemini-pro-agent"
    );
    assert_eq!(
        id("gemini-3.1-pro", Some(ReasoningLevel::Low)),
        "gemini-3.1-pro-low"
    );
    assert_eq!(
        id("gemini-3.1-pro", Some(ReasoningLevel::Medium)),
        "gemini-pro-agent"
    );
    assert_eq!(id("gemini-3.1-pro", None), "gemini-pro-agent");
    assert_eq!(
        id("gemini-3.7-flash", Some(ReasoningLevel::Medium)),
        "gemini-3.7-flash-medium"
    );
    assert_eq!(
        id("gemini-pro-agent", Some(ReasoningLevel::Low)),
        "gemini-pro-agent"
    );
    assert_eq!(
        id("gemini-legacy", Some(ReasoningLevel::High)),
        "gemini-legacy"
    );
    assert_eq!(id("unknown-model", None), "unknown-model");
}

#[test]
fn skills_list_from_their_frontmatter_first_folder_winning() {
    let first = tempfile::tempdir().unwrap();
    let second = tempfile::tempdir().unwrap();
    let skill = |dir: &std::path::Path, folder: &str, text: &str| {
        std::fs::create_dir_all(dir.join(folder)).unwrap();
        std::fs::write(dir.join(folder).join("SKILL.md"), text).unwrap();
    };
    skill(
        first.path(),
        "animate",
        "---\nname: animate\ndescription: Build an animation. Use when asked to animate.\n---\n# body",
    );
    skill(
        first.path(),
        "brandkit",
        "---\nname: \"brandkit\"\ndescription: >\n  Brand boards and\n  logo systems\n---\n",
    );
    skill(
        first.path(),
        "no-name",
        "---\ndescription: missing name\n---\n",
    );
    std::fs::create_dir_all(first.path().join("empty-folder")).unwrap();
    skill(
        second.path(),
        "animate",
        "---\nname: animate\ndescription: shadowed duplicate\n---\n",
    );
    skill(
        second.path(),
        "zeta",
        "---\nname: zeta\ndescription: Last one\n---\n",
    );

    let commands = skill_commands(&[
        first.path().to_path_buf(),
        PathBuf::from("/nonexistent/skills"),
        second.path().to_path_buf(),
    ]);
    let listed: Vec<(&str, &str)> = commands
        .iter()
        .map(|c| (c.name.as_str(), c.description.as_str()))
        .collect();
    assert_eq!(
        listed,
        vec![
            ("animate", "Build an animation."),
            ("brandkit", "Brand boards and logo systems"),
            ("zeta", "Last one"),
        ]
    );
}

#[test]
fn command_scan_finds_nested_advertisements() {
    let init = json!({
        "protocolVersion": 1,
        "agentCapabilities": {
            "_meta": {
                "availableCommands": [
                    { "name": "compact", "description": "Compact the session" },
                ],
            },
        },
    });
    let commands = scan_available_commands(&init);
    assert_eq!(commands.len(), 1);
    assert_eq!(commands[0].name, "compact");
    assert!(scan_available_commands(&json!({ "protocolVersion": 1 })).is_empty());
}

#[cfg(all(test, unix))]
#[tokio::test]
async fn setup_retains_bounded_session_metadata_before_response() {
    let mut child = Command::new(
        PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/fake-robust-acp.py"),
    )
    .stdin(Stdio::piped())
    .stdout(Stdio::piped())
    .kill_on_drop(true)
    .spawn()
    .unwrap();
    let (client, mut incoming) =
        RpcClient::new(child.stdin.take().unwrap(), child.stdout.take().unwrap());
    let result = tokio::time::timeout(
        Duration::from_secs(5),
        request_draining(&client, &mut incoming, "session/new", json!({})),
    )
    .await
    .unwrap()
    .unwrap();
    assert_eq!(result["availableCommands"][0]["name"], "early");
    assert_eq!(result["configOptions"], json!([]));
    assert_eq!(result["modes"]["currentModeId"], "plan");
    child.kill().await.unwrap();
}

#[cfg(all(test, unix))]
#[test]
fn explicit_program_launches_do_not_get_archive_scratch_roots() {
    let harness = AcpHarness::antigravity().with_executable(
        PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/fake-antigravity-acp.sh"),
    );
    assert!(harness.adapter_scratch().unwrap().is_none());
}

#[test]
fn prompt_response_cumulative_usage_does_not_emit_context_usage() {
    let res = Ok(json!({
        "stopReason": "end_turn",
        "usage": {
            "inputTokens": 428_192,
            "outputTokens": 9_626,
            "totalTokens": 16_734_637,
        }
    }));
    assert_eq!(context_usage_from_response(&res), None);
    assert_eq!(
        usage_from_response(&res),
        Some(AgentEvent::Usage {
            input_tokens: 428_192,
            output_tokens: 9_626,
        })
    );
}

#[test]
fn prompt_response_explicit_context_occupancy_is_preserved() {
    let res = Ok(json!({
        "stopReason": "end_turn",
        "usage": {
            "used": 158_946,
            "size": 1_048_576,
            "totalTokens": 2_006_197,
        }
    }));
    assert_eq!(
        context_usage_from_response(&res),
        Some(AgentEvent::ContextUsage {
            tokens: Some(158_946),
            window: Some(1_048_576),
        })
    );
}
