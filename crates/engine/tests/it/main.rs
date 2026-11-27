//! Engine integration suite. Run with nextest for process isolation.

#[path = "../../../../test_support/isolation.rs"]
mod test_isolation;

#[path = "../acp_lifecycle.rs"]
mod acp_lifecycle;
#[path = "../actual_model.rs"]
mod actual_model;
#[path = "../agent_tools.rs"]
mod agent_tools;
#[path = "../artifacts.rs"]
mod artifacts;
#[path = "../auth.rs"]
mod auth;
#[path = "../born_chat2_race.rs"]
mod born_chat2_race;
#[path = "../briefing_delivery.rs"]
mod briefing_delivery;
#[path = "../chat_workspace_plan_rpc.rs"]
mod chat_workspace_plan_rpc;
#[path = "../codex_login_resolution.rs"]
mod codex_login_resolution;
#[path = "../codex_login_resolver.rs"]
mod codex_login_resolver;
#[path = "../codex_subagents.rs"]
mod codex_subagents;
#[path = "../context_usage.rs"]
mod context_usage;
#[path = "../cursor_live.rs"]
mod cursor_live;
#[path = "../device_routing.rs"]
mod device_routing;
#[path = "../diff_sync_churn.rs"]
mod diff_sync_churn;
#[path = "../diff_sync_repair.rs"]
mod diff_sync_repair;
#[path = "../diff_targets.rs"]
mod diff_targets;
#[path = "../diff_targets_sync.rs"]
mod diff_targets_sync;
#[path = "../e2e.rs"]
mod e2e;
#[path = "../external_import.rs"]
mod external_import;
#[path = "../external_import_auto_adopt.rs"]
mod external_import_auto_adopt;
#[path = "../external_import_bulk_session_canvas.rs"]
mod external_import_bulk_session_canvas;
#[path = "../external_import_config_repair.rs"]
mod external_import_config_repair;
#[path = "../external_import_junk_filter.rs"]
mod external_import_junk_filter;
#[path = "../external_import_legacy_cursor.rs"]
mod external_import_legacy_cursor;
#[path = "../external_import_links_repair.rs"]
mod external_import_links_repair;
#[path = "../external_import_real_smoke.rs"]
mod external_import_real_smoke;
#[path = "../external_import_scheduled_tasks.rs"]
mod external_import_scheduled_tasks;
#[path = "../external_import_sync.rs"]
mod external_import_sync;
#[path = "../external_import_sync_stale.rs"]
mod external_import_sync_stale;
#[path = "../external_import_timestamp_repair.rs"]
mod external_import_timestamp_repair;
#[path = "../external_import_title_repair.rs"]
mod external_import_title_repair;
#[path = "../external_import_title_repair_harness_session.rs"]
mod external_import_title_repair_harness_session;
#[path = "../external_import_title_repair_no_transcript.rs"]
mod external_import_title_repair_no_transcript;
#[path = "../external_import_titles.rs"]
mod external_import_titles;
#[path = "../friction_rpc.rs"]
mod friction_rpc;
#[path = "../github_cli_login_shell.rs"]
mod github_cli_login_shell;
#[path = "../instruction_delivery.rs"]
mod instruction_delivery;
#[path = "../linear.rs"]
mod linear;
#[path = "../linear_assigned.rs"]
mod linear_assigned;
#[path = "../local_first.rs"]
mod local_first;
#[path = "../local_import.rs"]
mod local_import;
#[path = "../local_profiles.rs"]
mod local_profiles;
#[path = "../m5_repos_diffs_terminals.rs"]
mod m5_repos_diffs_terminals;
#[path = "../m5c_accounts_uploads_titles.rs"]
mod m5c_accounts_uploads_titles;
#[path = "../managed_accounts.rs"]
mod managed_accounts;
#[path = "../managed_usage.rs"]
mod managed_usage;
#[path = "../message_queue.rs"]
mod message_queue;
#[path = "../orchestrator_wakes.rs"]
mod orchestrator_wakes;
#[path = "../perf_bench.rs"]
mod perf_bench;
#[path = "../perf_startup_memory.rs"]
mod perf_startup_memory;
#[path = "../pi_resume.rs"]
mod pi_resume;
#[path = "../pr_review_launch.rs"]
mod pr_review_launch;
#[path = "../pr_review_sync.rs"]
mod pr_review_sync;
#[path = "../pr_ticket_cache_login_shell.rs"]
mod pr_ticket_cache_login_shell;
#[path = "../previews.rs"]
mod previews;
#[path = "../project_action_commands.rs"]
mod project_action_commands;
#[path = "../project_resync.rs"]
mod project_resync;
#[path = "../projectless.rs"]
mod projectless;
#[path = "../promote_saved_accounts.rs"]
mod promote_saved_accounts;
#[path = "../queued_attachments.rs"]
mod queued_attachments;
#[path = "../ref_diff_targets.rs"]
mod ref_diff_targets;
#[path = "../relay_delivery.rs"]
mod relay_delivery;
#[path = "../restart_resume.rs"]
mod restart_resume;
#[path = "../scheduled_followup.rs"]
mod scheduled_followup;
#[path = "../scheduled_tasks.rs"]
mod scheduled_tasks;
#[path = "../self_continued_quiesce.rs"]
mod self_continued_quiesce;
#[path = "../session_publication.rs"]
mod session_publication;
#[path = "../side_chat.rs"]
mod side_chat;
#[path = "../subagent_scan_harness_session.rs"]
mod subagent_scan_harness_session;
#[path = "../subagent_scan_harness_session_real_smoke.rs"]
mod subagent_scan_harness_session_real_smoke;
#[path = "../subagent_scan_real_smoke.rs"]
mod subagent_scan_real_smoke;
#[path = "../token_usage_rpc.rs"]
mod token_usage_rpc;
#[path = "../transcript_salvage.rs"]
mod transcript_salvage;
#[path = "../turn_quiesce.rs"]
mod turn_quiesce;
#[path = "../unarchive_on_send.rs"]
mod unarchive_on_send;
#[path = "../work_outline_execution_rpc.rs"]
mod work_outline_execution_rpc;
#[path = "../work_outline_instructions_rpc.rs"]
mod work_outline_instructions_rpc;
#[path = "../work_outline_kill_switch.rs"]
mod work_outline_kill_switch;
#[path = "../work_outline_launch.rs"]
mod work_outline_launch;
#[path = "../work_outline_pr_links.rs"]
mod work_outline_pr_links;
#[path = "../work_outline_rpc.rs"]
mod work_outline_rpc;
#[path = "../work_outline_user_actions_rpc.rs"]
mod work_outline_user_actions_rpc;
#[path = "../workspace_files.rs"]
mod workspace_files;
#[path = "../worker_context_window.rs"]
mod worker_context_window;
#[path = "../role_default_model.rs"]
mod role_default_model;
#[path = "../credit_fallback.rs"]
mod credit_fallback;
#[path = "../call_nudge.rs"]
mod call_nudge;
#[path = "../workspace_sync.rs"]
mod workspace_sync;
#[path = "../worktree_on_run.rs"]
mod worktree_on_run;

#[path = "../diff_sync_attach_gap.rs"]
mod diff_sync_attach_gap;
#[path = "../diff_sync_attach_measurement.rs"]
mod diff_sync_attach_measurement;
#[path = "../diff_sync_attach_reprobe.rs"]
mod diff_sync_attach_reprobe;
#[path = "../diff_sync_idle_budget.rs"]
mod diff_sync_idle_budget;
#[path = "../diff_sync_repair_backoff.rs"]
mod diff_sync_repair_backoff;
#[path = "../diff_sync_startup_budget.rs"]
mod diff_sync_startup_budget;
#[path = "../diff_sync_submodule_backoff.rs"]
mod diff_sync_submodule_backoff;
#[path = "../idle_spawn_budget.rs"]
mod idle_spawn_budget;
#[path = "../provenance_budget.rs"]
mod provenance_budget;
#[path = "../review_since.rs"]
mod review_since;
#[path = "../soak_seed.rs"]
mod soak_seed;
