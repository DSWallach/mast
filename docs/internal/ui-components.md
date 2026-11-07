# UI component inventory (in progress)

This is a preliminary index of style-specific references, not a complete component or call-site inventory. Line numbers refer to integration commit `99e0cd38d`. References include declarations and comparisons, not just branches.

## Target appearance model (not implemented yet)

Style, layout, and theme are independent settings. Style selects element design:
Default, Industrial, or Area. Layout selects density and guidance: Minimal,
Compact, or Guided. Theme supplies semantic colours. Components must resolve
look tokens and variants from style, density and guidance from layout, and
colours from the selected theme; no pairing may be assumed. Screens must not
branch on either style or layout. The planned lint must cover both.

The existing `uiStyle` combines these axes. Migration must map `minimal` to
Default + Minimal, `dense` to Default + Compact, `guided` to Default + Guided,
`industrial` to Industrial + Minimal, and `area` to Area + Minimal, then
persist separate `uiStyle` and `uiLayout` values. An explicit `uiLayout` must
remain authoritative on subsequent loads. Appearance needs separate Style,
Layout and Theme pickers. Gallery and preview need independent style and layout
axes, including the requested gallery keys and `--layouts` preview option.

## Token audit

`UiStyle::resolve` supplies look roles and independently applies layout metrics
and behavior. Type roles include an independent display field; radius roles
include an independent bubble field. The spacing scale derives from the
existing control gap and layout. The remaining optional role fallbacks are
intentional: Default/Minimal preserves each original primitive's geometry.

| Component | Default fallback when the look leaves a role unset |
| --- | --- |
| Button | Existing small/medium radii and type metrics |
| Card | Existing 12 px panel radius |
| Menu item | Existing menu radius and 13 px body size |
| Search and dialog input | Existing 8 px control radius |
| Readout | Existing 10 px label size |
| User bubble | Existing `Theme::BUBBLE_RADIUS` and 14 px body size |
| Guided section help | Existing 12 px secondary size |

The identity-color fallback in selected rows is a contrast fallback, not a
size token. The component migration slices must continue replacing their own
legacy metrics with these roles as they adopt components.

## Audited helper examples

Counts below are textual call expressions at commit `d9f3253b8`; test calls
are called out separately. The inventory below remains a candidate index and
is not an exhaustive inventory of inline controls.

| Target | Definition | Calls | Screen owner |
| --- | --- | ---: | --- |
| Icon button | `crates/ui/src/shell.rs:12112` `header_icon_button` | 4 production, 2 test | Shell tabs (`shell/tabs.rs`) |
| Button | `crates/ui/src/linear/panel.rs:1086` `small_button` | 12 | Linear panel and project picker |
| Button | `crates/ui/src/linear/settings_page.rs:164` `button` | 4 | Linear settings |
| Button | `crates/ui/src/work_outline/required_actions_view.rs:920` `button` | 12 | Required Actions |
| Icon button | `crates/ui/src/browser/view.rs:8` `button` | 4 | Browser |
| Section header | `crates/ui/src/work_outline/details_panel.rs:1205` `section_header` | 2 | Work item details |
| Button (migrated) | `crates/ui/src/work_outline/hints.rs` `details_button` | 93 | Details and its submodules |

## Named control candidates and call-site counts

This mechanical index lists a filtered set of functions whose names match control vocabulary, including tests, state logic and false positives. Counts are textual call expressions across `crates/ui/src`, excluding one declaration per entry; overloaded names can share a count. It does **not** identify inline ad-hoc elements, so the component inventory remains incomplete and needs a manual screen-by-screen audit. Locations refer to the current branch and will move with edits.

### Icon button

- `crates/ui/src/shell.rs:12112` `header_icon_button` — 6 textual calls

### Button

- `crates/ui/src/chat_closeout.rs:216` `button_label` — 5 textual calls
- `crates/ui/src/composer.rs:597` `send_button_mode` — 7 textual calls
- `crates/ui/src/composer.rs:6432` `render_slash_attach_button` — 1 textual calls
- `crates/ui/src/composer.rs:6742` `button_mode` — 4 textual calls
- `crates/ui/src/composer.rs:8097` `render_send_button` — 1 textual calls
- `crates/ui/src/composer.rs:11013` `send_button_morph` — 0 textual calls
- `crates/ui/src/chat_type_view.rs:736` `review_button` — 4 textual calls
- `crates/ui/src/shell.rs:11928` `window_control_button` — 3 textual calls
- `crates/ui/src/shell.rs:11992` `windows_caption_button` — 3 textual calls
- `crates/ui/src/shell.rs:12038` `linux_caption_button` — 3 textual calls
- `crates/ui/src/shell.rs:12083` `nav_history_button` — 2 textual calls
- `crates/ui/src/shell.rs:13839` `header_buttons_render_for_each_style` — 0 textual calls
- `crates/ui/src/transcript.rs:4332` `jump_button_shown` — 3 textual calls
- `crates/ui/src/transcript.rs:9021` `jump_button_stays_available_when_scrolling_down_until_near_bottom` — 0 textual calls
- `crates/ui/src/theme.rs:2531` `solid_button_is_legible_in_both_appearances` — 0 textual calls
- `crates/ui/src/queue.rs:986` `queue_primary_action_button` — 1 textual calls
- `crates/ui/src/changes.rs:2949` `history_fetch_button` — 1 textual calls
- `crates/ui/src/changes.rs:2972` `history_view_button` — 1 textual calls
- `crates/ui/src/changes.rs:5001` `header_button` — 4 textual calls
- `crates/ui/src/overview.rs:3390` `archive_button` — 1 textual calls
- `crates/ui/src/overview.rs:4534` `unlink_button` — 2 textual calls
- `crates/ui/src/overview.rs:4884` `render_new_chat_button` — 2 textual calls
- `crates/ui/src/artifacts/options_tests.rs:131` `options_open_in_synced_columns_with_a_pick_button_each` — 0 textual calls
- `crates/ui/src/artifacts/tab/comments_view.rs:742` `action_button` — 6 textual calls
- `crates/ui/src/artifacts/tab/compare_view.rs:1594` `compare_button` — 8 textual calls
- `crates/ui/src/shell/running_ui.rs:1207` `running_button` — 12 textual calls
- `crates/ui/src/shell/team_ui.rs:729` `team_button` — 4 textual calls
- `crates/ui/src/shell/tabs.rs:566` `chat_identity_button_label` — 3 textual calls
- `crates/ui/src/shell/tabs.rs:575` `chat_identity_button` — 1 textual calls
- `crates/ui/src/shell/tabs.rs:674` `identity_button_has_a_persistent_action_label` — 0 textual calls
- `crates/ui/src/shell/work_outline_ui.rs:1749` `confirm_button_reports_true_and_cancel_reports_false` — 0 textual calls
- `crates/ui/src/shell/work_outline_ui.rs:1985` `overview_pull_requests_button_opens_the_pr_page` — 0 textual calls
- `crates/ui/src/shell/pr_review_ui.rs:1070` `the_refresh_button_forces` — 0 textual calls
- `crates/ui/src/markdown/render.rs:566` `answer_button` — 3 textual calls
- `crates/ui/src/markdown/render.rs:2542` `code_copy_button` — 2 textual calls
- `crates/ui/src/linear/settings_page.rs:164` `button` — 62 textual calls
- `crates/ui/src/linear/panel.rs:1086` `small_button` — 23 textual calls
- `crates/ui/src/work_outline/templates_tests.rs:534` `the_toast_undo_button_undoes_the_creation` — 0 textual calls
- `crates/ui/src/work_outline/required_actions_view.rs:920` `button` — 62 textual calls
- `crates/ui/src/work_outline/required_actions_view.rs:950` `reply_button` — 1 textual calls
- `crates/ui/src/work_outline/hints.rs:24` `details_button` — 93 textual calls
- `crates/ui/src/work_outline/hints.rs:509` `details_button_hint` — 5 textual calls
- `crates/ui/src/work_outline/required_actions_tests.rs:1017` `container_template_button_shows_and_applies_proposal` — 0 textual calls
- `crates/ui/src/work_outline/required_actions_tests.rs:1205` `typed_action_button_sets_follow_request_kind` — 0 textual calls
- `crates/ui/src/work_outline/required_actions_tests.rs:1230` `choice_button_relays_and_resolves_once` — 0 textual calls
- `crates/ui/src/browser/fragment_view.rs:428` `status_button` — 2 textual calls
- `crates/ui/src/browser/view.rs:8` `button` — 62 textual calls
- `crates/ui/src/browser/linux/mod.rs:427` `mouse_button` — 1 textual calls
- `crates/ui/src/github/panel/sync_bar.rs:350` `stop_button` — 2 textual calls
- `crates/ui/src/files/mod.rs:45` `toolbar_button` — 9 textual calls
- `crates/ui/src/overview/linear_host.rs:869` `linear_toolbar_button` — 1 textual calls

### Toggle

- `crates/ui/src/chat_type_view.rs:1088` `filtering_uses_archived_toggle_and_skips_subagent_chats` — 0 textual calls
- `crates/ui/src/shell.rs:5930` `start_synced_switch` — 3 textual calls
- `crates/ui/src/shell.rs:5984` `drive_sync_switch` — 1 textual calls
- `crates/ui/src/shell.rs:13438` `local_sign_in_offers_the_in_place_switch` — 0 textual calls
- `crates/ui/src/shell.rs:13593` `switch_lifecycle_survives_the_runtime_replacement_window` — 0 textual calls
- `crates/ui/src/shell.rs:13628` `synced_sign_out_blocks_every_viewport_and_cannot_switch_accounts` — 0 textual calls
- `crates/ui/src/shell.rs:15633` `fixture_toggle_sidebar` — 0 textual calls
- `crates/ui/src/orchestrator_wakes.rs:365` `toggles_flip_one_switch_into_set_params` — 0 textual calls
- `crates/ui/src/changes.rs:5012` `header_toggle` — 3 textual calls
- `crates/ui/src/changes.rs:5058` `split_toggle` — 3 textual calls
- `crates/ui/src/changes.rs:5072` `wrap_toggle` — 3 textual calls
- `crates/ui/src/pickers.rs:1532` `toggle` — 44 textual calls
- `crates/ui/src/pickers.rs:2008` `switch_draft_ref` — 1 textual calls
- `crates/ui/src/state.rs:6035` `projectless_selection_clears_project_and_survives_device_switch` — 0 textual calls
- `crates/ui/src/ui_style.rs:631` `style_switch_preserves_selected_themes` — 0 textual calls
- `crates/ui/src/settings.rs:3419` `local_synced_local_switch_restores_each_profiles_pins` — 0 textual calls
- `crates/ui/src/settings.rs:3434` `account_switch_restores_each_accounts_pins` — 0 textual calls
- `crates/ui/src/settings.rs:4164` `linear_toggle_takes_its_default_from_the_first_unbound_release` — 0 textual calls
- `crates/ui/src/overview.rs:7581` `repo_filter_toggle_all_none_and_label` — 0 textual calls
- `crates/ui/src/overview.rs:10048` `inheritance_toggle_and_association_confirm_only_when_effective_changes` — 0 textual calls
- `crates/ui/src/composer_dock.rs:445` `panel_handoff_hides_background_during_geometry_switch_in_both_sidebar_states` — 0 textual calls
- `crates/ui/src/settings/composer.rs:263` `favorites_toggle_and_persist` — 0 textual calls
- `crates/ui/src/settings/harnesses.rs:530` `toggle` — 44 textual calls
- `crates/ui/src/settings/notifications.rs:60` `interactive_switch` — 8 textual calls
- `crates/ui/src/settings/notifications.rs:119` `toggle` — 44 textual calls
- `crates/ui/src/shell/files_panel.rs:497` `pane_toggle_drives_surfaces_only_and_last_tab_close_collapses_them` — 0 textual calls
- `crates/ui/src/shell/chat_item_tests.rs:1419` `the_team_header_has_wake_switches_and_shows_held_and_unseen_only_when_present` — 0 textual calls
- `crates/ui/src/shell/chat_item_tests.rs:1536` `toggling_a_wake_switch_updates_the_cached_settings_at_once` — 0 textual calls
- `crates/ui/src/shell/chat_item_tests.rs:1663` `archived_workers_leave_the_rows_and_a_toggle_brings_them_back` — 0 textual calls
- `crates/ui/src/shell/work_outline_ui.rs:3098` `linear_key_toggles_the_panel_beside_the_chat_panel` — 0 textual calls
- `crates/ui/src/shell/pr_review_ui.rs:900` `hide_drafts_group_toggle_and_filters` — 0 textual calls
- `crates/ui/src/shell/wake_ui.rs:174` `wake_toggle_chip` — 2 textual calls
- `crates/ui/src/linear/project_picker.rs:232` `toggle` — 44 textual calls
- `crates/ui/src/terminal/emulator.rs:592` `app_cursor_and_bracketed_paste_modes_toggle` — 0 textual calls
- `crates/ui/src/work_outline/templates_tests.rs:775` `friction_auto_capture_toggle_calls_engine` — 0 textual calls
- `crates/ui/src/work_outline/linear_tests.rs:254` `space_root_toggle_and_connect_prompt` — 0 textual calls
- `crates/ui/src/work_outline/project_ui_tests.rs:7` `the_project_toggle_and_worker_destination_send_updates_and_are_not_undoable` — 0 textual calls
- `crates/ui/src/work_outline/nav.rs:291` `expand_and_collapse_toggle_then_move` — 0 textual calls
- `crates/ui/src/files/markdown_preview.rs:2406` `resolved_question_cards_start_collapsed_and_expand_on_toggle` — 0 textual calls
- `crates/ui/src/files/mod.rs:1152` `explorer_header_search_and_visibility_toggle` — 0 textual calls
- `crates/ui/src/files/search.rs:144` `toggle` — 44 textual calls
- `crates/ui/src/files/search.rs:893` `checkout_switch_and_newer_reveal_reject_old_search_results` — 0 textual calls
- `crates/ui/src/composer_dock/panel_handoff.rs:60` `geometry_switch_is_hidden_in_both_directions` — 0 textual calls

### Chip and badge

- `crates/ui/src/notice.rs:32` `notice_chip` — 2 textual calls
- `crates/ui/src/composer.rs:5300` `render_comments_chip` — 1 textual calls
- `crates/ui/src/composer.rs:9881` `mention_tooltip_stays_visible_over_chip_or_popup_only` — 0 textual calls
- `crates/ui/src/composer.rs:10249` `projection_maps_and_expands_atomic_chip_ranges` — 0 textual calls
- `crates/ui/src/shell.rs:6754` `shortcut_badge` — 1 textual calls
- `crates/ui/src/shell.rs:15728` `tab_strip_scrolls_over_chips_inside_titlebar` — 0 textual calls
- `crates/ui/src/managed_accounts.rs:66` `chat_account_badge` — 11 textual calls
- `crates/ui/src/managed_accounts.rs:186` `start_chat_chip` — 7 textual calls
- `crates/ui/src/managed_accounts.rs:407` `chat_account_badge` — 11 textual calls
- `crates/ui/src/managed_accounts.rs:1381` `chat_badge_only_for_pinned_managed_accounts` — 0 textual calls
- `crates/ui/src/managed_accounts.rs:1448` `start_chat_chip_shows_the_project_default_and_marks_overrides` — 0 textual calls
- `crates/ui/src/managed_accounts.rs:1468` `start_chat_chip_is_quiet_without_accounts_to_choose_from` — 0 textual calls
- `crates/ui/src/transcript.rs:7783` `error_chip` — 1 textual calls
- `crates/ui/src/transcript.rs:7802` `input_chip` — 1 textual calls
- `crates/ui/src/transcript.rs:7882` `file_badge_name` — 7 textual calls
- `crates/ui/src/transcript.rs:8154` `chip_header_row` — 3 textual calls
- `crates/ui/src/transcript.rs:8419` `chip_header` — 1 textual calls
- `crates/ui/src/transcript.rs:8610` `tool_chip` — 1 textual calls
- `crates/ui/src/transcript.rs:8675` `subagent_chip` — 1 textual calls
- `crates/ui/src/transcript.rs:9832` `file_badge_icon_well_counter_shades_each_appearance` — 0 textual calls
- `crates/ui/src/transcript.rs:9856` `file_badge_icon_well_uses_more_coverage_on_frost` — 0 textual calls
- `crates/ui/src/transcript.rs:11417` `stray_subagent_ref_on_a_run_chip_stays_an_ordinary_tool` — 0 textual calls
- `crates/ui/src/transcript.rs:13690` `opening_an_edit_chip_prefetches_only_its_diff_sidecar` — 0 textual calls
- `crates/ui/src/transcript.rs:13894` `tool_chip_labels_per_kind` — 0 textual calls
- `crates/ui/src/transcript.rs:13949` `multiline_command_flattens_to_one_chip_line` — 0 textual calls
- `crates/ui/src/status_light.rs:174` `badge` — 34 textual calls
- `crates/ui/src/status_light.rs:350` `working_alone_is_working_with_no_badge` — 0 textual calls
- `crates/ui/src/status_light.rs:361` `typed_handoff_and_fyi_control_light_and_badge` — 0 textual calls
- `crates/ui/src/status_light.rs:420` `the_badge_counts_waiting_chats_errors_and_every_open_request` — 0 textual calls
- `crates/ui/src/orchestrator_wakes.rs:123` `held_chip_text` — 3 textual calls
- `crates/ui/src/orchestrator_wakes.rs:307` `chip_and_reason_text` — 0 textual calls
- `crates/ui/src/orchestrator_wakes.rs:335` `control_shows_chip_only_with_held_events` — 0 textual calls
- `crates/ui/src/change_requests.rs:113` `pull_request_badge` — 2 textual calls
- `crates/ui/src/change_requests.rs:122` `pull_request_badge_with_query` — 2 textual calls
- `crates/ui/src/change_requests.rs:133` `pull_request_badge_preview` — 2 textual calls
- `crates/ui/src/change_requests.rs:142` `render_pull_request_badge` — 2 textual calls
- `crates/ui/src/change_requests.rs:664` `badge_models_cover_open_merged_and_closed` — 0 textual calls
- `crates/ui/src/chat_item.rs:50` `chip` — 40 textual calls
- `crates/ui/src/pickers.rs:178` `chip_label` — 7 textual calls
- `crates/ui/src/pickers.rs:1065` `model_chip_focus` — 3 textual calls
- `crates/ui/src/pickers.rs:1069` `approval_chip_focus` — 2 textual calls
- `crates/ui/src/pickers.rs:1235` `chip_model_label` — 3 textual calls
- `crates/ui/src/pickers.rs:1401` `render_approval_chip` — 1 textual calls
- `crates/ui/src/pickers.rs:2593` `workspace_chip_eligible` — 2 textual calls
- `crates/ui/src/pickers.rs:2625` `render_workspace_chip` — 2 textual calls
- `crates/ui/src/pickers.rs:3780` `trigger_chip` — 1 textual calls
- `crates/ui/src/pickers.rs:3893` `footer_chip` — 7 textual calls
- `crates/ui/src/pickers.rs:5806` `default_badge` — 1 textual calls
- `crates/ui/src/pickers.rs:7585` `chip_test_chat` — 1 textual calls
- `crates/ui/src/pickers.rs:7624` `existing_chat_chip_marks_the_preselected_default_as_the_next_turn` — 0 textual calls
- `crates/ui/src/history.rs:1128` `estimated_ref_badge_width` — 0 textual calls
- `crates/ui/src/history.rs:5212` `ref_badges_preserve_overflow_when_the_first_badge_is_too_wide` — 0 textual calls
- `crates/ui/src/comments.rs:161` `extract_badge` — 4 textual calls
- `crates/ui/src/comments.rs:249` `chip_label` — 7 textual calls
- `crates/ui/src/comments.rs:362` `chip_label_pluralizes` — 0 textual calls
- `crates/ui/src/appshots.rs:84` `badge` — 34 textual calls
- `crates/ui/src/settings.rs:1802` `badge_combo` — 8 textual calls
- `crates/ui/src/settings.rs:1807` `badge_combo_on` — 7 textual calls
- `crates/ui/src/settings.rs:4039` `badge_combos_use_mac_glyphs_and_linux_text` — 0 textual calls
- `crates/ui/src/overview.rs:689` `model_badge` — 1 textual calls
- `crates/ui/src/overview.rs:917` `link_has_badge_row` — 0 textual calls
- `crates/ui/src/overview.rs:1304` `pr_action_badge` — 0 textual calls
- `crates/ui/src/admin.rs:149` `badge_text` — 5 textual calls
- `crates/ui/src/admin.rs:155` `badge_tooltip` — 3 textual calls
- `crates/ui/src/settings/widgets.rs:357` `badge` — 34 textual calls
- `crates/ui/src/settings/widgets.rs:363` `badge_active` — 3 textual calls
- `crates/ui/src/artifacts/questions_tests.rs:255` `drift_badge_shows_for_unreflected_answers_and_commits_only` — 0 textual calls
- `crates/ui/src/artifacts/questions_ui.rs:53` `drift_badge` — 6 textual calls
- `crates/ui/src/artifacts/questions_ui.rs:243` `badge_for_unreflected_answers_only` — 0 textual calls
- `crates/ui/src/artifacts/questions_ui.rs:261` `badge_for_commits_only` — 0 textual calls
- `crates/ui/src/artifacts/questions_ui.rs:269` `badge_for_both_and_none` — 0 textual calls
- `crates/ui/src/artifacts/tests.rs:561` `header_shows_edited_without_a_note_and_the_plan_badge` — 0 textual calls
- `crates/ui/src/artifacts/tab.rs:1754` `chip` — 40 textual calls
- `crates/ui/src/artifacts/tab/questions_view.rs:512` `badge` — 34 textual calls
- `crates/ui/src/shell/work_outline_menu_ui.rs:2208` `chat_account_badge_state_only_for_a_pinned_managed_account` — 0 textual calls
- `crates/ui/src/shell/chat_item_tests.rs:213` `the_header_shows_breadcrumb_title_chips_and_badges` — 0 textual calls
- `crates/ui/src/shell/chat_item_tests.rs:344` `the_badges_open_the_item_tab_and_the_pane_opens_with_it` — 0 textual calls
- `crates/ui/src/shell/chat_item_tests.rs:664` `the_header_pr_chip_opens_a_side_tab_and_focuses_it_the_second_time` — 0 textual calls
- `crates/ui/src/shell/chat_item_tests.rs:819` `header_chips_carry_icons_and_the_title_stacks_under_the_breadcrumb` — 0 textual calls
- `crates/ui/src/shell/chat_item_tests.rs:856` `chip_tones_use_theme_tokens` — 0 textual calls
- `crates/ui/src/shell/team_ui.rs:83` `render_team_tab_chip` — 1 textual calls
- `crates/ui/src/shell/team_ui.rs:276` `chip` — 40 textual calls
- `crates/ui/src/shell/admin_ui.rs:230` `render_admin_badge` — 2 textual calls
- `crates/ui/src/shell/chat_item_ui.rs:38` `chip_palette` — 8 textual calls
- `crates/ui/src/shell/chat_item_ui.rs:401` `render_header_badges` — 1 textual calls
- `crates/ui/src/shell/chat_item_ui.rs:469` `render_header_chips` — 1 textual calls
- `crates/ui/src/shell/chat_item_ui.rs:704` `render_item_tab_chip` — 1 textual calls
- `crates/ui/src/shell/pr_review_ui.rs:843` `tab_badges_show_urgent_over_total_or_just_the_total` — 0 textual calls
- `crates/ui/src/shell/pr_review_ui.rs:1110` `a_linked_chat_chip_opens_that_chat` — 0 textual calls
- `crates/ui/src/shell/admin_tests.rs:171` `the_badge_shows_for_live_grants_only_and_revoke_sends_revoke_admin` — 0 textual calls
- `crates/ui/src/shell/start_chat_ui.rs:1080` `start_chat_account_chip` — 2 textual calls
- `crates/ui/src/shell/start_chat_ui.rs:2918` `start_chat_account_chip_overrides_the_project_default_for_one_chat` — 0 textual calls
- `crates/ui/src/linear/panel/space_menu.rs:84` `render_space_chip` — 1 textual calls
- `crates/ui/src/admin/tests.rs:66` `badge_text_names_the_space_and_vanishes_at_expiry` — 0 textual calls
- `crates/ui/src/admin/tests.rs:316` `grant_and_revoke_send_their_rpcs_and_the_badge_state_follows` — 0 textual calls
- `crates/ui/src/work_outline/instructions.rs:41` `provenance_badge` — 3 textual calls
- `crates/ui/src/work_outline/linear_tests.rs:1354` `project_chip_opens_a_menu_instead_of_cycling` — 0 textual calls
- `crates/ui/src/work_outline/linear_tests.rs:1413` `issue_chip_text_is_stable_across_refreshes_and_changes_with_the_data` — 0 textual calls
- `crates/ui/src/work_outline/chips.rs:38` `path_chip` — 1 textual calls
- `crates/ui/src/work_outline/chips.rs:218` `template_chip` — 2 textual calls
- `crates/ui/src/work_outline/template_review_tests.rs:160` `row_chip` — 5 textual calls
- `crates/ui/src/work_outline/template_review_tests.rs:184` `the_chip_shows_only_when_the_linked_revision_is_behind` — 0 textual calls
- `crates/ui/src/work_outline/template_review_tests.rs:224` `a_missing_revision_counts_as_zero_and_archived_projects_show_no_chip` — 0 textual calls
- `crates/ui/src/work_outline/template_review_tests.rs:408` `accept_all_applies_every_change_then_the_settled_snapshot_clears_the_chip` — 0 textual calls
- `crates/ui/src/work_outline/outline_view.rs:2718` `artifact_count_chip` — 1 textual calls
- `crates/ui/src/work_outline/outline_view.rs:2776` `drift_chip` — 2 textual calls
- `crates/ui/src/work_outline/outline_view.rs:3365` `proposals_chip` — 3 textual calls
- `crates/ui/src/work_outline/hints.rs:478` `kbd_chip` — 3 textual calls
- `crates/ui/src/work_outline/linear_outline.rs:239` `fix_link_chip` — 2 textual calls
- `crates/ui/src/work_outline/linear_outline.rs:280` `issue_chip_text` — 2 textual calls
- `crates/ui/src/work_outline/linear_outline.rs:603` `issue_chip` — 1 textual calls
- `crates/ui/src/work_outline/linear_outline.rs:653` `project_chip` — 4 textual calls
- `crates/ui/src/work_outline/compact_rows_tests.rs:468` `waiting_item_shows_a_row_chip` — 0 textual calls
- `crates/ui/src/work_outline/required_actions_tests.rs:991` `space_proposal_chip_opens_container_proposals` — 0 textual calls
- `crates/ui/src/work_outline/pr_sync_tests.rs:968` `pr_chip_text_is_stable_across_refreshes_and_changes_with_the_data` — 0 textual calls
- `crates/ui/src/work_outline/pr_sync_tests.rs:1120` `pr_rows_show_icon_slots_instead_of_chip_text` — 0 textual calls
- `crates/ui/src/work_outline/template_review.rs:33` `chip_revisions` — 4 textual calls
- `crates/ui/src/work_outline/template_review.rs:49` `chip_text` — 14 textual calls
- `crates/ui/src/github/chip.rs:88` `chip_text` — 14 textual calls
- `crates/ui/src/github/chip.rs:171` `pr_chip` — 3 textual calls
- `crates/ui/src/github/chip.rs:245` `chip_text_names_state_ci_and_my_review` — 0 textual calls
- `crates/ui/src/chat_type_view/pr_tabs.rs:1094` `dash_badge` — 4 textual calls
- `crates/ui/src/pr_page/mod.rs:188` `badge` — 34 textual calls
- `crates/ui/src/pr_page/mod.rs:317` `chip_text` — 14 textual calls
- `crates/ui/src/pr_page/mod.rs:520` `chat_chip_label` — 2 textual calls
- `crates/ui/src/pr_page/mod.rs:696` `badge_is_urgent_over_admitted_rows_only` — 0 textual calls
- `crates/ui/src/pr_page/mod.rs:931` `chat_chip_labels_truncate_and_link_sources_have_text` — 0 textual calls

### Tabs and segmented controls

- `crates/ui/src/composer.rs:1168` `display_row_segments` — 3 textual calls
- `crates/ui/src/composer.rs:2764` `mention_tab` — 0 textual calls
- `crates/ui/src/shell.rs:258` `stable_panel_content_width` — 5 textual calls
- `crates/ui/src/shell.rs:3361` `browser_tab_showing` — 1 textual calls
- `crates/ui/src/shell.rs:10960` `render_right_tab_strip` — 2 textual calls
- `crates/ui/src/shell.rs:13816` `file_surface_is_single_instance_per_tab_list` — 0 textual calls
- `crates/ui/src/shell.rs:15757` `subagent_tab_body_still_selects_drags_and_middle_closes` — 0 textual calls
- `crates/ui/src/shell.rs:15788` `subagent_tab_click_jitter_selects_without_starting_a_drag` — 0 textual calls
- `crates/ui/src/transcript.rs:8471` `subagent_tab_title` — 7 textual calls
- `crates/ui/src/transcript.rs:10152` `viewport_anchor_tracks_a_stable_row_across_replay` — 0 textual calls
- `crates/ui/src/transcript.rs:13844` `subagent_tab_titles` — 0 textual calls
- `crates/ui/src/changes.rs:2396` `tab_title` — 3 textual calls
- `crates/ui/src/pickers.rs:410` `segment_target` — 7 textual calls
- `crates/ui/src/pickers.rs:5821` `tab_indicator` — 2 textual calls
- `crates/ui/src/pickers.rs:7745` `tab_search_never_leaves_the_viewed_harness` — 0 textual calls
- `crates/ui/src/pickers.rs:7775` `favorites_tab_search_ranks_only_starred_rows` — 0 textual calls
- `crates/ui/src/pickers.rs:7816` `harness_tab_lists_stars_first_and_description_still_matches` — 0 textual calls
- `crates/ui/src/pickers.rs:8086` `segment_target_resolution` — 0 textual calls
- `crates/ui/src/history.rs:1024` `point_to_segment_distance` — 2 textual calls
- `crates/ui/src/history.rs:1046` `segment_distance` — 1 textual calls
- `crates/ui/src/settings.rs:2301` `team_tab_visibility_defaults_for_fresh_and_existing_profiles` — 0 textual calls
- `crates/ui/src/settings.rs:2320` `team_tab_visibility_modes` — 0 textual calls
- `crates/ui/src/settings.rs:3683` `team_tab_shortcut_persists_and_detects_conflicts` — 0 textual calls
- `crates/ui/src/overview.rs:420` `last_segment` — 2 textual calls
- `crates/ui/src/artifacts/links.rs:198` `legacy_artifact_scheme_still_opens_an_artifact_tab` — 0 textual calls
- `crates/ui/src/artifacts/find_tests.rs:64` `focus_tab` — 13 textual calls
- `crates/ui/src/artifacts/find_tests.rs:121` `cmd_f_works_from_any_focus_inside_the_tab_but_not_outside_it` — 0 textual calls
- `crates/ui/src/artifacts/find_tests.rs:810` `section_keys_do_nothing_outside_a_plan_view_and_outside_the_tab` — 0 textual calls
- `crates/ui/src/artifacts/options_tests.rs:288` `plan_tab` — 3 textual calls
- `crates/ui/src/artifacts/tests.rs:496` `compare_source_tab_uses_the_revision_text_diff` — 0 textual calls
- `crates/ui/src/shell/artifacts_ui.rs:30` `artifact_tab_title` — 1 textual calls
- `crates/ui/src/shell/artifacts_ui.rs:37` `artifact_tab` — 3 textual calls
- `crates/ui/src/shell/artifacts_ui.rs:226` `focus_artifact_tab` — 2 textual calls
- `crates/ui/src/shell/artifacts_ui.rs:491` `artifact_links_open_a_tab_by_id_and_register_unknown_paths` — 0 textual calls
- `crates/ui/src/shell/artifacts_ui.rs:588` `cmd_f_is_find_in_a_focused_artifact_tab_and_outline_search_elsewhere` — 0 textual calls
- `crates/ui/src/shell/chat_item_tests.rs:279` `the_item_tab_shows_the_item_sheet` — 0 textual calls
- `crates/ui/src/shell/chat_item_tests.rs:363` `the_item_tab_stays_selected_over_other_tabs_until_one_is_picked` — 0 textual calls
- `crates/ui/src/shell/chat_item_tests.rs:554` `the_item_tab_lists_identifiers_with_copy_for_chats_with_and_without_an_item` — 0 textual calls
- `crates/ui/src/shell/chat_item_tests.rs:693` `the_item_tab_pr_link_follows_the_same_rules` — 0 textual calls
- `crates/ui/src/shell/chat_item_tests.rs:838` `item_tab_sections_lead_with_icons` — 0 textual calls
- `crates/ui/src/shell/chat_item_tests.rs:871` `the_item_tab_keeps_rows_tight_and_actions_together` — 0 textual calls
- `crates/ui/src/shell/chat_item_tests.rs:1008` `the_item_tab_has_no_other_sections_expanded_until_asked` — 0 textual calls
- `crates/ui/src/shell/chat_item_tests.rs:1049` `the_item_tab_stays_readable_with_maximal_data` — 0 textual calls
- `crates/ui/src/shell/chat_item_tests.rs:1131` `a_chat_without_workers_has_no_team_tab_and_the_key_does_nothing` — 0 textual calls
- `crates/ui/src/shell/chat_item_tests.rs:1142` `the_team_tab_lists_a_worker_per_row_with_state_activity_and_a_roll_up` — 0 textual calls
- `crates/ui/src/shell/chat_item_tests.rs:1162` `team_tab_rows_stack_without_overlap_for_two_and_fifteen_workers` — 0 textual calls
- `crates/ui/src/shell/chat_item_tests.rs:1189` `plain_t_opens_the_team_tab_only_when_the_panel_itself_is_focused` — 0 textual calls
- `crates/ui/src/shell/chat_item_tests.rs:1232` `the_team_hotkey_avoids_the_browsers_new_tab` — 0 textual calls
- `crates/ui/src/shell/chat_item_tests.rs:1349` `a_long_title_truncates_inside_the_tab_and_the_card` — 0 textual calls
- `crates/ui/src/shell/chat_item_tests.rs:1517` `the_item_tab_shows_the_compact_wake_control_only_for_chats_with_workers` — 0 textual calls
- `crates/ui/src/shell/chat_item_tests.rs:1604` `the_team_tab_offers_archive_finished_and_suggests_it_from_three` — 0 textual calls
- `crates/ui/src/shell/chat_item_tests.rs:1711` `required_action_structured_references_render_on_the_item_tab` — 0 textual calls
- `crates/ui/src/shell/team_ui.rs:148` `render_team_tab` — 2 textual calls
- `crates/ui/src/shell/team_ui.rs:230` `render_item_tab_team` — 1 textual calls
- `crates/ui/src/shell/admin_ui.rs:298` `render_item_tab_admin` — 1 textual calls
- `crates/ui/src/shell/tabs.rs:685` `tabs_align_with_the_panel_for_each_caption_layout_and_files_width` — 0 textual calls
- `crates/ui/src/shell/chat_item_ui.rs:754` `render_item_tab` — 2 textual calls
- `crates/ui/src/shell/chat_item_ui.rs:922` `render_item_tab_sections` — 1 textual calls
- `crates/ui/src/shell/pr_review_ui.rs:1041` `the_visible_tab_refetches_every_minute_only_while_on_screen` — 0 textual calls
- `crates/ui/src/shell/pr_review_ui.rs:1635` `concurrent_fetches_of_one_tab_make_one_request` — 0 textual calls
- `crates/ui/src/shell/wake_ui.rs:360` `render_item_tab_wake` — 1 textual calls
- `crates/ui/src/shell/actions_ui.rs:1019` `action_segment` — 3 textual calls
- `crates/ui/src/shell/start_chat_ui.rs:820` `start_chat_tab` — 4 textual calls
- `crates/ui/src/markdown/render.rs:3600` `table_header_flattens_at_weight_700` — 0 textual calls
- `crates/ui/src/markdown/parser.rs:1028` `tables_parse_header_and_rows` — 0 textual calls
- `crates/ui/src/terminal/panel.rs:448` `tab_summaries` — 1 textual calls
- `crates/ui/src/terminal/panel.rs:473` `reserve_tab_for_chat` — 3 textual calls
- `crates/ui/src/terminal/panel.rs:532` `fail_reserved_tab` — 1 textual calls
- `crates/ui/src/terminal/panel.rs:610` `ensure_tab` — 2 textual calls
- `crates/ui/src/terminal/panel.rs:619` `tab_mut` — 13 textual calls
- `crates/ui/src/terminal/panel.rs:627` `active_tab` — 4 textual calls
- `crates/ui/src/terminal/panel.rs:1253` `sync_rail_tab` — 1 textual calls
- `crates/ui/src/terminal/panel.rs:1261` `rail_tab` — 1 textual calls
- `crates/ui/src/terminal/panel.rs:1268` `rail_tab_mut` — 1 textual calls
- `crates/ui/src/terminal/panel.rs:1411` `render_tab_bar` — 3 textual calls
- `crates/ui/src/work_outline/restructure_tests.rs:789` `tab_and_shift_tab_alias_indent_and_outdent` — 0 textual calls
- `crates/ui/src/work_outline/outline_view.rs:3803` `compact_chat_segment` — 1 textual calls
- `crates/ui/src/browser/model.rs:202` `tab_labels_fall_back_to_host_then_browser` — 0 textual calls
- `crates/ui/src/chat_type_view/pr_tabs.rs:282` `render_tab_bar` — 3 textual calls
- `crates/ui/src/files/mod.rs:775` `tab_title` — 3 textual calls
- `crates/ui/src/files/search.rs:827` `selecting_a_tab_preserves_search_and_never_opens_another_file` — 0 textual calls
- `crates/ui/src/pr_page/mod.rs:61` `tab_index` — 24 textual calls

### Text input

- `crates/ui/src/composer.rs:166` `input_content_height` — 6 textual calls
- `crates/ui/src/composer.rs:195` `input_max_scroll` — 8 textual calls
- `crates/ui/src/composer.rs:201` `input_overflow_edges` — 5 textual calls
- `crates/ui/src/composer.rs:216` `input_reveal_height` — 5 textual calls
- `crates/ui/src/composer.rs:226` `input_scroll_offset` — 6 textual calls
- `crates/ui/src/composer.rs:236` `input_scroll_offset_for_cursor` — 8 textual calls
- `crates/ui/src/composer.rs:290` `input_drag_scroll_delta` — 6 textual calls
- `crates/ui/src/composer.rs:646` `pending_input_request` — 8 textual calls
- `crates/ui/src/composer.rs:668` `input_request_resolved` — 4 textual calls
- `crates/ui/src/composer.rs:1431` `message_input_context` — 3 textual calls
- `crates/ui/src/composer.rs:1454` `input_bindings` — 3 textual calls
- `crates/ui/src/composer.rs:6062` `render_input_with_completion` — 3 textual calls
- `crates/ui/src/composer.rs:9278` `palette_search_context_input_deletes_with_backspace_and_chords` — 0 textual calls
- `crates/ui/src/composer.rs:10457` `input_wheel_scroll_uses_gpui_direction_and_clamps` — 0 textual calls
- `crates/ui/src/composer.rs:10469` `input_scroll_reveals_only_when_caret_leaves_viewport` — 0 textual calls
- `crates/ui/src/composer.rs:10492` `input_drag_autoscroll_is_edge_proportional_and_capped` — 0 textual calls
- `crates/ui/src/composer.rs:10718` `resize_keeps_text_anchored_to_the_input_origin` — 0 textual calls
- `crates/ui/src/composer.rs:11139` `pending_input_detection` — 0 textual calls
- `crates/ui/src/chat_type_view.rs:357` `focus_review_input` — 1 textual calls
- `crates/ui/src/chat_type_view.rs:381` `review_input` — 1 textual calls
- `crates/ui/src/chat_type_view.rs:386` `input_status` — 1 textual calls
- `crates/ui/src/chat_type_view.rs:435` `submit_review_input` — 2 textual calls
- `crates/ui/src/shell.rs:15856` `explicit_blur_does_not_refocus_a_mounted_input` — 0 textual calls
- `crates/ui/src/transcript.rs:12388` `runway_background_burst_and_downward_input_reach_the_new_tail` — 0 textual calls
- `crates/ui/src/theme.rs:1048` `input_glass_bg` — 5 textual calls
- `crates/ui/src/theme.rs:2754` `input_plate_never_reads_as_recessed` — 0 textual calls
- `crates/ui/src/sound.rs:501` `pending_send_consumes_completion_but_preserves_input_requests` — 0 textual calls
- `crates/ui/src/status_light.rs:381` `awaiting_input_is_input_and_beats_ready_and_working` — 0 textual calls
- `crates/ui/src/status_light.rs:393` `any_other_open_request_is_input_even_without_a_waiting_chat` — 0 textual calls
- `crates/ui/src/surface_chrome.rs:47` `input` — 51 textual calls
- `crates/ui/src/popover.rs:1061` `search_input_frame` — 15 textual calls
- `crates/ui/src/overview.rs:7077` `link_input_parses_github_pr_urls_and_normalizes` — 0 textual calls
- `crates/ui/src/overview.rs:7095` `link_input_parses_ticket_ids_and_linear_urls` — 0 textual calls
- `crates/ui/src/overview.rs:7106` `link_input_rejects_garbage` — 0 textual calls
- `crates/ui/src/overview_grouping.rs:258` `zero_dimensions_is_flat_leaves_in_input_order` — 0 textual calls
- `crates/ui/src/image_media.rs:362` `encoded_input_and_decode_dimensions_are_bounded` — 0 textual calls
- `crates/ui/src/composer_dock.rs:491` `panel_return_sizes_while_hidden_and_finishes_controls_with_input` — 0 textual calls
- `crates/ui/src/artifacts/questions_tests.rs:204` `clicking_the_open_answer_input_keeps_focus_and_typing_lands` — 0 textual calls
- `crates/ui/src/artifacts/tab/zoom_view.rs:99` `apply_zoom_input` — 4 textual calls
- `crates/ui/src/artifacts/tab/options_view.rs:341` `note_input` — 4 textual calls
- `crates/ui/src/artifacts/tab/find_view.rs:218` `nav_input_for_tests` — 1 textual calls
- `crates/ui/src/shell/instructions_ui.rs:283` `input_focus` — 9 textual calls
- `crates/ui/src/shell/hand_off_ui.rs:19` `input_focus` — 9 textual calls
- `crates/ui/src/shell/start_chat_ui.rs:109` `input_focus` — 9 textual calls
- `crates/ui/src/composer/skill_completion.rs:628` `sync_input` — 3 textual calls
- `crates/ui/src/linear/masked_input.rs:183` `masks_input_and_drops_whitespace` — 0 textual calls
- `crates/ui/src/terminal/panel.rs:848` `queue_input` — 2 textual calls
- `crates/ui/src/terminal/panel.rs:881` `flush_input` — 1 textual calls
- `crates/ui/src/work_outline/details_panel.rs:1290` `input_frame` — 13 textual calls
- `crates/ui/src/work_outline/undo_tests.rs:165` `cmd_z_in_a_text_field_does_not_undo_the_outline` — 0 textual calls
- `crates/ui/src/work_outline/required_actions_view.rs:450` `inline_reply_input` — 1 textual calls
- `crates/ui/src/work_outline/required_actions_view.rs:455` `note_input` — 4 textual calls
- `crates/ui/src/work_outline/compact_rows_tests.rs:450` `cmd_i_in_a_text_field_does_nothing_outline_related` — 0 textual calls
- `crates/ui/src/work_outline/details_execution.rs:95` `new_ref_input` — 2 textual calls
- `crates/ui/src/browser/model.rs:160` `rejects_non_web_schemes_credentials_and_bad_input` — 0 textual calls
- `crates/ui/src/browser/linux/mod.rs:555` `accepts_text_input` — 0 textual calls
- `crates/ui/src/chat_type_view/pr_tabs.rs:1122` `dash_search_input` — 1 textual calls
- `crates/ui/src/menu_bar/tests.rs:12` `input` — 51 textual calls
- `crates/ui/src/running_chats/tests.rs:8` `input` — 51 textual calls

### Section header and label

- `crates/ui/src/chat_closeout.rs:283` `dirty_count_label` — 5 textual calls
- `crates/ui/src/chat_closeout.rs:315` `unmerged_label` — 4 textual calls
- `crates/ui/src/chat_closeout.rs:328` `unverifiable_label` — 6 textual calls
- `crates/ui/src/chat_closeout.rs:343` `dirty_inspection_label` — 2 textual calls
- `crates/ui/src/workspace_chip.rs:50` `label` — 230 textual calls
- `crates/ui/src/workspace_chip.rs:229` `label` — 230 textual calls
- `crates/ui/src/diff_targets.rs:20` `label` — 230 textual calls
- `crates/ui/src/running_chats.rs:38` `label` — 230 textual calls
- `crates/ui/src/running_chats.rs:388` `outcome_label` — 2 textual calls
- `crates/ui/src/team.rs:40` `label` — 230 textual calls
- `crates/ui/src/team.rs:80` `label` — 230 textual calls
- `crates/ui/src/composer.rs:946` `escape_mention_label` — 2 textual calls
- `crates/ui/src/composer.rs:1031` `label_close` — 1 textual calls
- `crates/ui/src/composer.rs:4090` `slash_attach_key_label` — 2 textual calls
- `crates/ui/src/chat_type_view.rs:116` `repo_label` — 1 textual calls
- `crates/ui/src/chat_type_view.rs:170` `status_label` — 2 textual calls
- `crates/ui/src/shell.rs:593` `label` — 230 textual calls
- `crates/ui/src/shell.rs:901` `sidebar_faded_label` — 6 textual calls
- `crates/ui/src/shell.rs:4688` `ensure_chat_header_links` — 2 textual calls
- `crates/ui/src/shell.rs:4741` `invalidate_chat_header_links` — 1 textual calls
- `crates/ui/src/shell.rs:6765` `new_session_shortcut_label` — 1 textual calls
- `crates/ui/src/shell.rs:13860` `guided_header_labels_collapse_when_space_is_limited` — 0 textual calls
- `crates/ui/src/shell.rs:14614` `copy_chat_identity_writes_clipboard_and_flashes_header_feedback` — 0 textual calls
- `crates/ui/src/project_actions.rs:241` `show_action_label` — 3 textual calls
- `crates/ui/src/managed_accounts.rs:94` `menu_label` — 4 textual calls
- `crates/ui/src/transcript.rs:7888` `tool_patch_file_header` — 1 textual calls
- `crates/ui/src/theme.rs:81` `label` — 230 textual calls
- `crates/ui/src/orchestrator_wakes.rs:43` `label` — 230 textual calls
- `crates/ui/src/orchestrator_wakes.rs:409` `stopped_worker_has_a_clear_unseen_label` — 0 textual calls
- `crates/ui/src/changes.rs:1021` `uncommitted_label` — 4 textual calls
- `crates/ui/src/changes.rs:1060` `label` — 230 textual calls
- `crates/ui/src/changes.rs:1085` `scope_label` — 8 textual calls
- `crates/ui/src/changes.rs:1761` `sticky_file_header` — 12 textual calls
- `crates/ui/src/changes.rs:1790` `sticky_header_push_offset` — 6 textual calls
- `crates/ui/src/changes.rs:1826` `sticky_file_header_paint` — 4 textual calls
- `crates/ui/src/changes.rs:4665` `render_submodule_header` — 1 textual calls
- `crates/ui/src/changes.rs:4793` `render_file_header` — 2 textual calls
- `crates/ui/src/changes.rs:4935` `render_sticky_file_header` — 1 textual calls
- `crates/ui/src/changes.rs:5097` `render_header_controls` — 5 textual calls
- `crates/ui/src/changes.rs:5903` `render_header_strip` — 1 textual calls
- `crates/ui/src/changes.rs:6208` `hunk_header_row` — 3 textual calls
- `crates/ui/src/changes.rs:7447` `sticky_header_tracks_the_logical_top_row` — 0 textual calls
- `crates/ui/src/changes.rs:7510` `sticky_header_is_pushed_by_the_next_file` — 0 textual calls
- `crates/ui/src/changes.rs:7522` `sticky_header_uses_the_content_theme_in_dark_and_light` — 0 textual calls
- `crates/ui/src/changes.rs:8623` `header_label_pluralizes` — 0 textual calls
- `crates/ui/src/menu_bar.rs:215` `short_label` — 1 textual calls
- `crates/ui/src/edge_fade.rs:145` `fade_label_overflow` — 2 textual calls
- `crates/ui/src/edge_fade.rs:276` `label_fade_outset` — 5 textual calls
- `crates/ui/src/edge_fade.rs:290` `painted_label_retains_scroll_fade_across_overflow_transitions` — 0 textual calls
- `crates/ui/src/edge_fade.rs:404` `label_fade_enters_continuously_and_settles_at_one_band` — 0 textual calls
- `crates/ui/src/chat_item.rs:397` `header_hover` — 11 textual calls
- `crates/ui/src/typography.rs:26` `label` — 230 textual calls
- `crates/ui/src/typography.rs:102` `label` — 230 textual calls
- `crates/ui/src/pickers.rs:273` `reasoning_label` — 2 textual calls
- `crates/ui/src/pickers.rs:1211` `selected_model_label` — 4 textual calls
- `crates/ui/src/pickers.rs:2719` `checkout_label` — 2 textual calls
- `crates/ui/src/pickers.rs:2737` `ref_label` — 8 textual calls
- `crates/ui/src/pickers.rs:3951` `footer_label` — 3 textual calls
- `crates/ui/src/history.rs:464` `history_column_label` — 1 textual calls
- `crates/ui/src/history.rs:3053` `render_column_header_cell` — 1 textual calls
- `crates/ui/src/appearance.rs:43` `label` — 230 textual calls
- `crates/ui/src/chat_metadata.rs:214` `source_label` — 4 textual calls
- `crates/ui/src/chat_metadata.rs:538` `header_shows_the_model_that_ran_not_a_default` — 0 textual calls
- `crates/ui/src/popover.rs:965` `key_hint_label` — 3 textual calls
- `crates/ui/src/ui_style.rs:16` `label` — 230 textual calls
- `crates/ui/src/ui_style.rs:60` `label` — 230 textual calls
- `crates/ui/src/review_chats.rs:162` `state_label` — 17 textual calls
- `crates/ui/src/review_chats.rs:173` `marker_label` — 3 textual calls
- `crates/ui/src/appshots.rs:263` `label` — 230 textual calls
- `crates/ui/src/settings.rs:94` `label` — 230 textual calls
- `crates/ui/src/settings.rs:1077` `label` — 230 textual calls
- `crates/ui/src/overview.rs:138` `label` — 230 textual calls
- `crates/ui/src/overview.rs:178` `turn_role_label` — 3 textual calls
- `crates/ui/src/overview.rs:455` `group_label` — 5 textual calls
- `crates/ui/src/overview.rs:644` `repo_filter_label` — 6 textual calls
- `crates/ui/src/overview.rs:683` `chat_model_label` — 6 textual calls
- `crates/ui/src/overview.rs:1044` `link_source_label` — 8 textual calls
- `crates/ui/src/overview.rs:1132` `header_links` — 1 textual calls
- `crates/ui/src/overview.rs:1364` `context_pct_label` — 5 textual calls
- `crates/ui/src/overview.rs:5303` `lane_header` — 1 textual calls
- `crates/ui/src/overview.rs:8320` `context_label_and_fill_match_reference_face` — 0 textual calls
- `crates/ui/src/overview.rs:8392` `card_model_label_uses_explicit_id_or_honest_default` — 0 textual calls
- `crates/ui/src/overview.rs:10366` `long_instructions_collapse_and_controls_sit_in_the_header` — 0 textual calls
- `crates/ui/src/admin.rs:53` `label` — 230 textual calls
- `crates/ui/src/admin.rs:109` `grant_entry_label` — 5 textual calls
- `crates/ui/src/settings/composer.rs:147` `label_for` — 1 textual calls
- `crates/ui/src/settings/harnesses.rs:154` `pending_label` — 3 textual calls
- `crates/ui/src/settings/harnesses.rs:161` `failure_label` — 3 textual calls
- `crates/ui/src/settings/shortcuts.rs:465` `modifier_send_label` — 4 textual calls
- `crates/ui/src/settings/devices.rs:231` `platform_label` — 1 textual calls
- `crates/ui/src/settings/appearance.rs:69` `label` — 230 textual calls
- `crates/ui/src/settings/appearance.rs:1235` `surface_label` — 2 textual calls
- `crates/ui/src/settings/widgets.rs:169` `page_header` — 10 textual calls
- `crates/ui/src/settings/widgets.rs:203` `field_label` — 10 textual calls
- `crates/ui/src/artifacts/find_tests.rs:75` `label` — 230 textual calls
- `crates/ui/src/artifacts/plan_linear.rs:59` `state_label` — 17 textual calls
- `crates/ui/src/artifacts/plan_linear.rs:78` `health_label` — 0 textual calls
- `crates/ui/src/artifacts/plan_linear.rs:88` `target_label` — 4 textual calls
- `crates/ui/src/artifacts/zoom.rs:124` `label` — 230 textual calls
- `crates/ui/src/artifacts/compare.rs:57` `label` — 230 textual calls
- `crates/ui/src/artifacts/questions_ui.rs:41` `age_label` — 16 textual calls
- `crates/ui/src/artifacts/questions_ui.rs:134` `changed_since_label` — 2 textual calls
- `crates/ui/src/artifacts/comments.rs:35` `label` — 230 textual calls
- `crates/ui/src/artifacts/tab.rs:910` `render_header` — 1 textual calls
- `crates/ui/src/artifacts/markdown_diff.rs:274` `section_label` — 25 textual calls
- `crates/ui/src/artifacts/find.rs:86` `label` — 230 textual calls
- `crates/ui/src/artifacts/find.rs:357` `the_first_match_is_current_and_the_label_counts` — 0 textual calls
- `crates/ui/src/artifacts/model.rs:90` `kind_label` — 1 textual calls
- `crates/ui/src/artifacts/model.rs:105` `age_label` — 16 textual calls
- `crates/ui/src/artifacts/model.rs:140` `header` — 14 textual calls
- `crates/ui/src/artifacts/model.rs:159` `revision_label` — 6 textual calls
- `crates/ui/src/artifacts/outline.rs:373` `empty_headings_take_a_slot_but_get_no_row` — 0 textual calls
- `crates/ui/src/artifacts/outline.rs:485` `the_page_reports_a_heading_position_and_empty_ones_resolve_to_the_previous_row` — 0 textual calls
- `crates/ui/src/artifacts/tab/options_view.rs:54` `option_label` — 2 textual calls
- `crates/ui/src/artifacts/tab/options_view.rs:491` `pick_label` — 2 textual calls
- `crates/ui/src/shell/spaces.rs:1089` `hidden_label_sidebar_section_drops_follow_pointer` — 0 textual calls
- `crates/ui/src/shell/spaces.rs:1769` `pinned_drag_accounts_for_disclosure_header_and_scroll` — 0 textual calls
- `crates/ui/src/shell/spaces.rs:2070` `sidebar_disclosure_header` — 4 textual calls
- `crates/ui/src/shell/chat_item_tests.rs:248` `the_old_strip_drift_and_proposal_lines_are_gone_from_the_header` — 0 textual calls
- `crates/ui/src/shell/chat_item_tests.rs:387` `hover_header` — 4 textual calls
- `crates/ui/src/shell/chat_item_tests.rs:441` `a_quick_pass_over_the_header_shows_nothing` — 0 textual calls
- `crates/ui/src/shell/chat_item_tests.rs:598` `a_chat_without_a_session_id_lists_no_session_row_and_the_header_has_no_identifiers` — 0 textual calls
- `crates/ui/src/shell/running_ui.rs:1197` `interrupted_label` — 2 textual calls
- `crates/ui/src/shell/chat_item_ui.rs:66` `section_label` — 25 textual calls
- `crates/ui/src/shell/chat_item_ui.rs:194` `chat_item_header_hover` — 1 textual calls
- `crates/ui/src/shell/chat_item_ui.rs:245` `render_chat_header_metadata` — 2 textual calls
- `crates/ui/src/shell/chat_item_ui.rs:338` `render_header_breadcrumb` — 1 textual calls
- `crates/ui/src/shell/work_outline_ui.rs:277` `label` — 230 textual calls
- `crates/ui/src/shell/wake_ui.rs:330` `render_team_wake_header` — 1 textual calls
- `crates/ui/src/shell/actions_ui.rs:1060` `action_field_label` — 3 textual calls
- `crates/ui/src/shell/start_chat_ui.rs:1057` `section_label` — 25 textual calls
- `crates/ui/src/markdown/question.rs:29` `label` — 230 textual calls
- `crates/ui/src/markdown/question.rs:166` `split_header` — 1 textual calls
- `crates/ui/src/markdown/question.rs:208` `label` — 230 textual calls
- `crates/ui/src/markdown/render.rs:2590` `code_block_header` — 1 textual calls
- `crates/ui/src/linear/panel.rs:1110` `render_section_header` — 2 textual calls
- `crates/ui/src/linear/prefs.rs:72` `label` — 230 textual calls
- `crates/ui/src/admin/tests.rs:346` `the_tick_notifies_only_when_a_label_or_the_grants_change` — 0 textual calls
- `crates/ui/src/work_outline/details_panel.rs:1205` `section_header` — 8 textual calls
- `crates/ui/src/work_outline/required_actions.rs:61` `label` — 230 textual calls
- `crates/ui/src/work_outline/required_actions.rs:243` `actor_label` — 2 textual calls
- `crates/ui/src/work_outline/required_actions.rs:416` `age_label` — 16 textual calls
- `crates/ui/src/work_outline/required_actions.rs:503` `active_label` — 2 textual calls
- `crates/ui/src/work_outline/required_actions.rs:508` `attention_label` — 2 textual calls
- `crates/ui/src/work_outline/required_actions.rs:513` `label` — 230 textual calls
- `crates/ui/src/work_outline/required_actions.rs:612` `proposals_label` — 4 textual calls
- `crates/ui/src/work_outline/instructions.rs:200` `confirm_label` — 2 textual calls
- `crates/ui/src/work_outline/instructions.rs:402` `revision_label` — 6 textual calls
- `crates/ui/src/work_outline/undo_tests.rs:101` `undo_label` — 8 textual calls
- `crates/ui/src/work_outline/details_pr_sync.rs:43` `request_state_label` — 1 textual calls
- `crates/ui/src/work_outline/command.rs:74` `label` — 230 textual calls
- `crates/ui/src/work_outline/execution.rs:103` `source_label` — 4 textual calls
- `crates/ui/src/work_outline/execution.rs:150` `ref_label` — 8 textual calls
- `crates/ui/src/work_outline/execution.rs:758` `label` — 230 textual calls
- `crates/ui/src/work_outline/chips.rs:192` `base_status_label` — 4 textual calls
- `crates/ui/src/work_outline/hints.rs:108` `key_label` — 18 textual calls
- `crates/ui/src/work_outline/hints.rs:113` `key_label_on` — 25 textual calls
- `crates/ui/src/work_outline/filter.rs:153` `path_label` — 18 textual calls
- `crates/ui/src/work_outline/filter.rs:272` `filter_label` — 1 textual calls
- `crates/ui/src/work_outline/details_execution.rs:54` `device_label` — 3 textual calls
- `crates/ui/src/work_outline/details_user_actions.rs:87` `chat_label` — 2 textual calls
- `crates/ui/src/work_outline/tree_guides_tests.rs:157` `label` — 230 textual calls
- `crates/ui/src/work_outline/tree_guides.rs:194` `drop_label` — 8 textual calls
- `crates/ui/src/work_outline/model.rs:861` `path_label` — 18 textual calls
- `crates/ui/src/work_outline/outline_view/tree_guides_view.rs:128` `drop_label` — 8 textual calls
- `crates/ui/src/work_outline/outline_view/tree_guides_view.rs:323` `drop_label_element` — 1 textual calls
- `crates/ui/src/work_outline/outline_view/undo.rs:211` `next_undo_label` — 12 textual calls
- `crates/ui/src/work_outline/outline_view/undo.rs:222` `next_redo_label` — 1 textual calls
- `crates/ui/src/browser/model.rs:15` `label` — 230 textual calls
- `crates/ui/src/github/chip.rs:16` `state_label` — 17 textual calls
- `crates/ui/src/github/chip.rs:36` `ci_label` — 3 textual calls
- `crates/ui/src/github/chip.rs:64` `review_decision_label` — 5 textual calls
- `crates/ui/src/github/chip.rs:75` `my_review_label` — 4 textual calls
- `crates/ui/src/github/panel.rs:535` `render_list_header` — 1 textual calls
- `crates/ui/src/github/prefs.rs:34` `label` — 230 textual calls
- `crates/ui/src/github/panel/menus.rs:128` `scope_label` — 8 textual calls
- `crates/ui/src/chat_type_view/pr_tabs.rs:583` `render_dash_header` — 1 textual calls
- `crates/ui/src/files/preview.rs:2147` `render_editor_header` — 1 textual calls
- `crates/ui/src/files/mod.rs:1042` `render_explorer_header` — 1 textual calls
- `crates/ui/src/pickers/approval.rs:42` `label` — 230 textual calls
- `crates/ui/src/pr_page/mod.rs:44` `label` — 230 textual calls
- `crates/ui/src/pr_page/mod.rs:313` `header_text` — 3 textual calls
- `crates/ui/src/overview/linear_host.rs:777` `column_header` — 1 textual calls

### Card and panel

- `crates/ui/src/comment_ui.rs:48` `render_comment_card` — 1 textual calls
- `crates/ui/src/shell.rs:262` `right_panel_content_width` — 5 textual calls
- `crates/ui/src/shell.rs:2852` `panel_key` — 40 textual calls
- `crates/ui/src/shell.rs:2981` `right_terminal_panel` — 4 textual calls
- `crates/ui/src/shell.rs:4309` `terminal_panel` — 4 textual calls
- `crates/ui/src/shell.rs:4790` `show_chat_in_overview_panel` — 2 textual calls
- `crates/ui/src/shell.rs:11497` `render_gate_card` — 1 textual calls
- `crates/ui/src/shell.rs:13297` `right_panel_content_keeps_the_larger_width_only_during_transition` — 0 textual calls
- `crates/ui/src/shell.rs:14349` `panel_saves_preserve_settings_selected_outside_the_shell` — 0 textual calls
- `crates/ui/src/managed_accounts.rs:910` `render_card` — 2 textual calls
- `crates/ui/src/transcript.rs:7588` `visualization_card` — 1 textual calls
- `crates/ui/src/theme.rs:954` `panel_bg` — 2 textual calls
- `crates/ui/src/theme.rs:1066` `card_glass_bg` — 2 textual calls
- `crates/ui/src/theme.rs:1754` `card_selected_bg` — 6 textual calls
- `crates/ui/src/theme.rs:1792` `card_selected_shadows` — 3 textual calls
- `crates/ui/src/theme.rs:1973` `inverse_panel_text_is_opaque_and_contrasts_in_industrial_light` — 0 textual calls
- `crates/ui/src/theme.rs:2612` `bare_plates_are_visible_against_their_panel` — 0 textual calls
- `crates/ui/src/theme.rs:2773` `card_selection_paints_nothing_behind_its_row` — 0 textual calls
- `crates/ui/src/queue.rs:224` `queue_panel_surface` — 2 textual calls
- `crates/ui/src/queue.rs:285` `render_queue_panel` — 1 textual calls
- `crates/ui/src/queue.rs:1890` `the_whole_panel_maps_to_a_clamped_queue_drop_slot` — 0 textual calls
- `crates/ui/src/chat_item.rs:405` `card_hover` — 3 textual calls
- `crates/ui/src/chat_item.rs:567` `the_card_waits_to_show_and_to_hide_and_stale_timers_do_nothing` — 0 textual calls
- `crates/ui/src/chat_item.rs:594` `pinning_keeps_the_card_until_released` — 0 textual calls
- `crates/ui/src/comments.rs:268` `card_body_lines` — 5 textual calls
- `crates/ui/src/comments.rs:275` `card_height` — 10 textual calls
- `crates/ui/src/comments.rs:410` `card_height_grows_with_body_lines` — 0 textual calls
- `crates/ui/src/popover.rs:324` `popover_card` — 41 textual calls
- `crates/ui/src/popover.rs:348` `popover_card_flush` — 3 textual calls
- `crates/ui/src/popover.rs:919` `palette_card` — 0 textual calls
- `crates/ui/src/popover.rs:1086` `dialog_card` — 36 textual calls
- `crates/ui/src/ui_style.rs:506` `help_card` — 5 textual calls
- `crates/ui/src/ui_style.rs:784` `help_card_dismissal_persists` — 0 textual calls
- `crates/ui/src/ui_style.rs:863` `guided_metrics_and_help_card_dismissals` — 0 textual calls
- `crates/ui/src/badges.rs:210` `the_card_carries_one_row_per_comment` — 0 textual calls
- `crates/ui/src/settings.rs:3449` `files_panel_width_defaults_roundtrips_and_clamps` — 0 textual calls
- `crates/ui/src/settings.rs:3611` `legacy_panel_defaults_migrate_and_custom_shortcuts_survive` — 0 textual calls
- `crates/ui/src/overview.rs:1424` `panel_chat_id` — 9 textual calls
- `crates/ui/src/overview.rs:2724` `copy_panel_chat_identity` — 2 textual calls
- `crates/ui/src/overview.rs:2757` `panel_chat_id` — 9 textual calls
- `crates/ui/src/overview.rs:3481` `render_chat_panel` — 2 textual calls
- `crates/ui/src/overview.rs:5253` `card_dimming` — 7 textual calls
- `crates/ui/src/overview.rs:6569` `shown_panel_chat` — 12 textual calls
- `crates/ui/src/overview.rs:6579` `return_to_outline_chat_panel` — 1 textual calls
- `crates/ui/src/overview.rs:6727` `show_list_with_panel_for_test` — 1 textual calls
- `crates/ui/src/overview.rs:6958` `required_actions_panel` — 2 textual calls
- `crates/ui/src/overview.rs:7314` `panel_chat_id_follows_selection_only_while_open` — 0 textual calls
- `crates/ui/src/overview.rs:8215` `embedded_chat_panel_exposes_and_copies_chat_identity` — 0 textual calls
- `crates/ui/src/composer_dock.rs:129` `return_from_panel` — 1 textual calls
- `crates/ui/src/composer_dock.rs:477` `panel_exit_retains_source_transcript_width_only_until_handoff_ends` — 0 textual calls
- `crates/ui/src/settings/appearance.rs:1581` `report_panel` — 2 textual calls
- `crates/ui/src/settings/appearance.rs:3578` `every_mode_gets_a_card` — 0 textual calls
- `crates/ui/src/settings/widgets.rs:213` `option_card_row` — 3 textual calls
- `crates/ui/src/settings/widgets.rs:237` `option_card` — 3 textual calls
- `crates/ui/src/settings/widgets.rs:286` `section_card` — 20 textual calls
- `crates/ui/src/settings/widgets.rs:292` `card_row` — 30 textual calls
- `crates/ui/src/artifacts/cards.rs:26` `card_model` — 4 textual calls
- `crates/ui/src/artifacts/cards.rs:80` `artifact_card` — 1 textual calls
- `crates/ui/src/artifacts/options_tests.rs:368` `the_panel_needs_the_pinned_plan_and_linear_on` — 0 textual calls
- `crates/ui/src/artifacts/question_html.rs:298` `script_builds_the_card_from_the_documented_attributes_as_text` — 0 textual calls
- `crates/ui/src/artifacts/question_html.rs:388` `web_documents_carry_the_card_style_and_script_around_the_authors_markup` — 0 textual calls
- `crates/ui/src/artifacts/tab/comments_view.rs:486` `with_comments_panel` — 1 textual calls
- `crates/ui/src/artifacts/tab/comments_view.rs:507` `render_comments_panel` — 1 textual calls
- `crates/ui/src/shell/instructions_ui.rs:1059` `compare_card` — 1 textual calls
- `crates/ui/src/shell/instructions_ui.rs:1241` `from_chat_card` — 1 textual calls
- `crates/ui/src/shell/files_panel.rs:17` `files_panel_layout` — 11 textual calls
- `crates/ui/src/shell/files_panel.rs:54` `files_panel_open` — 27 textual calls
- `crates/ui/src/shell/files_panel.rs:261` `render_files_panel` — 1 textual calls
- `crates/ui/src/shell/chat_item_tests.rs:408` `the_card_shows_after_the_hover_delay_and_hides_on_mouse_out` — 0 textual calls
- `crates/ui/src/shell/chat_item_tests.rs:451` `the_shortcut_pins_the_card_open_and_releases_it` — 0 textual calls
- `crates/ui/src/shell/chat_item_tests.rs:466` `the_card_never_takes_the_keyboard_from_the_composer` — 0 textual calls
- `crates/ui/src/shell/chat_item_tests.rs:493` `changing_chat_drops_the_card_and_its_pin` — 0 textual calls
- `crates/ui/src/shell/chat_item_tests.rs:1206` `team_panel_rebind_replaces_the_default` — 0 textual calls
- `crates/ui/src/shell/chat_item_tests.rs:1364` `the_hover_card_row_shows_the_same_status` — 0 textual calls
- `crates/ui/src/shell/hand_off_ui.rs:136` `hand_off_card` — 1 textual calls
- `crates/ui/src/shell/files_panel_workspace_tests.rs:69` `files_panel_workspace_navigation_and_external_updates` — 0 textual calls
- `crates/ui/src/shell/team_ui.rs:76` `team_panel_focus` — 5 textual calls
- `crates/ui/src/shell/team_ui.rs:197` `panel_root` — 2 textual calls
- `crates/ui/src/shell/tabs.rs:75` `panel_titlebar_widths` — 5 textual calls
- `crates/ui/src/shell/chat_item_ui.rs:176` `run_card_timer` — 2 textual calls
- `crates/ui/src/shell/chat_item_ui.rs:199` `chat_item_card_hover` — 1 textual calls
- `crates/ui/src/shell/chat_item_ui.rs:651` `render_item_card` — 1 textual calls
- `crates/ui/src/shell/work_outline_ui.rs:523` `shortcuts_card` — 1 textual calls
- `crates/ui/src/shell/work_outline_ui.rs:2642` `panel_chat` — 12 textual calls
- `crates/ui/src/shell/work_outline_ui.rs:3022` `enter_on_a_selected_list_row_opens_the_chat_panel` — 0 textual calls
- `crates/ui/src/shell/start_chat_ui.rs:1043` `error_card` — 6 textual calls
- `crates/ui/src/shell/start_chat_ui.rs:1181` `start_chat_card` — 1 textual calls
- `crates/ui/src/shell/start_chat_ui.rs:1642` `branch_start_chat_card` — 1 textual calls
- `crates/ui/src/shell/start_chat_ui.rs:1682` `pr_start_chat_card` — 1 textual calls
- `crates/ui/src/changes/explanation.rs:1065` `explanation_reply_renders_in_anchored_panel` — 0 textual calls
- `crates/ui/src/markdown/link_destination.rs:26` `destination_card` — 2 textual calls
- `crates/ui/src/markdown/render.rs:3034` `area_code_tokens_remain_readable_on_inverse_panel` — 0 textual calls
- `crates/ui/src/linear/prefs.rs:91` `panel_state_from_set` — 2 textual calls
- `crates/ui/src/linear/prefs.rs:105` `panel_state_to_set` — 2 textual calls
- `crates/ui/src/linear/prefs.rs:161` `panel_state_round_trips_and_ignores_unknown_entries` — 0 textual calls
- `crates/ui/src/terminal/view.rs:47` `terminal_panel_bg` — 2 textual calls
- `crates/ui/src/work_outline/resizable_panes_tests.rs:18` `panel_width` — 14 textual calls
- `crates/ui/src/work_outline/linear_tests.rs:277` `panel_lists_search_and_linked_marker` — 0 textual calls
- `crates/ui/src/work_outline/linear_tests.rs:672` `space_row_menu_opens_the_panel` — 0 textual calls
- `crates/ui/src/work_outline/linear_tests.rs:710` `linear_column_sits_left_of_the_body_beside_the_chat_panel` — 0 textual calls
- `crates/ui/src/work_outline/linear_tests.rs:969` `drag_from_the_left_column_creates_beside_the_chat_panel` — 0 textual calls
- `crates/ui/src/work_outline/required_actions_tests.rs:55` `handoff_card_displays_cached_lifecycle` — 0 textual calls
- `crates/ui/src/work_outline/required_actions_tests.rs:1290` `focused_choice_card_accepts_number_key` — 0 textual calls
- `crates/ui/src/files/preview.rs:2770` `render_editor_comment_card` — 1 textual calls
- `crates/ui/src/files/image_preview.rs:421` `rendered_workspace_image_click_keeps_zoom_in_the_file_panel` — 0 textual calls
- `crates/ui/src/menu_bar/tests.rs:516` `an_account_without_windows_says_why_in_its_card` — 0 textual calls
- `crates/ui/src/overview/github_host.rs:83` `ensure_github_panel` — 1 textual calls
- `crates/ui/src/overview/github_host.rs:107` `drop_github_panel` — 2 textual calls
- `crates/ui/src/overview/github_host.rs:113` `github_panel` — 1 textual calls
- `crates/ui/src/overview/linear_host.rs:185` `sync_github_panel` — 2 textual calls
- `crates/ui/src/overview/linear_host.rs:229` `linear_panel` — 15 textual calls
- `crates/ui/src/overview/linear_host.rs:259` `ensure_linear_panel` — 2 textual calls

### List row and menu item

- `crates/ui/src/composer.rs:4301` `slash_row_body` — 2 textual calls
- `crates/ui/src/composer.rs:9888` `mention_wash_moves_wholly_to_the_next_visual_row_at_a_wrap` — 0 textual calls
- `crates/ui/src/composer.rs:10952` `new_thread_selectors_restore_the_compact_floating_row` — 0 textual calls
- `crates/ui/src/composer.rs:10977` `modified_submit_sends_content_and_activates_latest_queue_row_when_empty` — 0 textual calls
- `crates/ui/src/chat_type_view.rs:128` `row_matches` — 5 textual calls
- `crates/ui/src/chat_type_view.rs:607` `render_row` — 24 textual calls
- `crates/ui/src/chat_type_view.rs:1037` `row` — 266 textual calls
- `crates/ui/src/shell.rs:863` `chat_row_height` — 5 textual calls
- `crates/ui/src/shell.rs:877` `sidebar_row_height` — 12 textual calls
- `crates/ui/src/shell.rs:7263` `render_chat_row` — 3 textual calls
- `crates/ui/src/shell.rs:13901` `sidebar_harness_geometry_reflects_row_hierarchy` — 0 textual calls
- `crates/ui/src/shell.rs:13922` `resort_offsets_activity_moves_row_to_top` — 0 textual calls
- `crates/ui/src/rail.rs:259` `scroll_to_row` — 1 textual calls
- `crates/ui/src/transcript.rs:2637` `tool_row_reveal_progress` — 7 textual calls
- `crates/ui/src/transcript.rs:3673` `remeasure_last_row` — 7 textual calls
- `crates/ui/src/transcript.rs:4028` `first_row_top_gap` — 7 textual calls
- `crates/ui/src/transcript.rs:6203` `render_row` — 24 textual calls
- `crates/ui/src/transcript.rs:8082` `more_lines_row` — 2 textual calls
- `crates/ui/src/transcript.rs:8493` `reveal_tool_row` — 3 textual calls
- `crates/ui/src/transcript.rs:9033` `top_inset_feeds_first_row_gap_and_resting_spot_consistently` — 0 textual calls
- `crates/ui/src/transcript.rs:9900` `live_row_parse_work_is_bounded_per_commit` — 0 textual calls
- `crates/ui/src/transcript.rs:10137` `viewport_row` — 39 textual calls
- `crates/ui/src/transcript.rs:10660` `generated_image_builds_one_row_outside_tools_and_copy` — 0 textual calls
- `crates/ui/src/transcript.rs:10690` `visualize_control_token_builds_an_artifact_row_and_preserves_surrounding_text` — 0 textual calls
- `crates/ui/src/transcript.rs:10721` `appending_visualize_token_keeps_streamed_markdown_row_ids_and_parser_key` — 0 textual calls
- `crates/ui/src/transcript.rs:11264` `live_commit_changes_only_tail_row_versions` — 0 textual calls
- `crates/ui/src/transcript.rs:13240` `expanding_a_prompt_is_not_a_row_change` — 0 textual calls
- `crates/ui/src/transcript.rs:13274` `relayed_row_renders_source_and_expands` — 0 textual calls
- `crates/ui/src/transcript.rs:14036` `timestamp_strip_lands_on_the_last_settled_row` — 0 textual calls
- `crates/ui/src/transcript.rs:14126` `tool_row_reveal_honors_delay_easing_and_reduced_motion` — 0 textual calls
- `crates/ui/src/queue.rs:381` `queue_row` — 1 textual calls
- `crates/ui/src/queue.rs:1828` `available_primary_action_obeys_row_and_host_gates` — 0 textual calls
- `crates/ui/src/queue.rs:1838` `queue_shortcut_only_appears_on_the_actionable_latest_row_when_revealed` — 0 textual calls
- `crates/ui/src/queue.rs:1847` `queue_shortcut_targets_the_most_recently_added_row` — 0 textual calls
- `crates/ui/src/queue.rs:1898` `drag_offsets_move_the_real_row_and_open_its_destination` — 0 textual calls
- `crates/ui/src/changes.rs:1545` `body_row_count` — 3 textual calls
- `crates/ui/src/changes.rs:4305` `render_row` — 24 textual calls
- `crates/ui/src/changes.rs:6193` `notice_row` — 3 textual calls
- `crates/ui/src/changes.rs:6283` `diff_line_row` — 3 textual calls
- `crates/ui/src/changes.rs:6423` `meta_line_row` — 3 textual calls
- `crates/ui/src/changes.rs:6570` `split_row` — 2 textual calls
- `crates/ui/src/changes.rs:7748` `a_split_row_offers_each_column_its_own_anchor` — 0 textual calls
- `crates/ui/src/changes.rs:9235` `removing_a_checkout_unbinds_it_and_removes_its_row` — 0 textual calls
- `crates/ui/src/pickers.rs:39` `workspace_footer_row` — 3 textual calls
- `crates/ui/src/pickers.rs:1057` `chat_row` — 8 textual calls
- `crates/ui/src/pickers.rs:2411` `activate_model_row` — 3 textual calls
- `crates/ui/src/pickers.rs:4504` `retry_row` — 5 textual calls
- `crates/ui/src/pickers.rs:5218` `render_model_row` — 1 textual calls
- `crates/ui/src/pickers.rs:8164` `default_model_is_first_catalog_row` — 0 textual calls
- `crates/ui/src/pickers.rs:8186` `default_model_prefers_the_mast_default_over_a_cli_default_row` — 0 textual calls
- `crates/ui/src/history.rs:3944` `render_row` — 24 textual calls
- `crates/ui/src/state.rs:1756` `selected_space_row` — 26 textual calls
- `crates/ui/src/state.rs:1795` `space_row` — 36 textual calls
- `crates/ui/src/state.rs:1934` `selected_chat_row` — 36 textual calls
- `crates/ui/src/state.rs:6153` `apply_chat_config_stamps_the_row` — 0 textual calls
- `crates/ui/src/popover.rs:837` `menu_row` — 69 textual calls
- `crates/ui/src/popover.rs:845` `menu_row_nav` — 23 textual calls
- `crates/ui/src/popover.rs:1255` `error_row` — 3 textual calls
- `crates/ui/src/badges.rs:108` `row` — 266 textual calls
- `crates/ui/src/overview.rs:632` `reconcile_hovered_row` — 4 textual calls
- `crates/ui/src/overview.rs:2219` `ensure_row_fetches` — 2 textual calls
- `crates/ui/src/overview.rs:2254` `next_row_fetch_due` — 1 textual calls
- `crates/ui/src/overview.rs:5494` `render_list_row` — 1 textual calls
- `crates/ui/src/overview.rs:7641` `hovered_row_reconciliation_drops_only_unmounted_ids` — 0 textual calls
- `crates/ui/src/overview.rs:8344` `row` — 266 textual calls
- `crates/ui/src/overview.rs:8513` `list_row_renders_scanned_subagent_rows_inside_its_bounds` — 0 textual calls
- `crates/ui/src/overview.rs:8604` `list_subagent_row_click_opens_readonly_peek` — 0 textual calls
- `crates/ui/src/overview.rs:8776` `list_row_identity_survives_source_reordering` — 0 textual calls
- `crates/ui/src/overview.rs:9105` `row_bounds` — 238 textual calls
- `crates/ui/src/settings/composer.rs:280` `remember_model_updates_harness_and_row` — 0 textual calls
- `crates/ui/src/settings/shortcuts.rs:272` `render_row` — 24 textual calls
- `crates/ui/src/settings/accounts.rs:1094` `render_account_row` — 1 textual calls
- `crates/ui/src/settings/accounts.rs:1450` `render_skeleton_row` — 2 textual calls
- `crates/ui/src/settings/appshots.rs:7` `row` — 266 textual calls
- `crates/ui/src/settings/widgets.rs:298` `row_tile` — 23 textual calls
- `crates/ui/src/settings/widgets.rs:318` `row_title` — 34 textual calls
- `crates/ui/src/artifacts/find_tests.rs:594` `clicking_a_rail_row_focuses_the_rail_and_enter_or_escape_leave_it` — 0 textual calls
- `crates/ui/src/artifacts/question_html.rs:279` `css_styles_every_status_and_the_decision_row` — 0 textual calls
- `crates/ui/src/artifacts/model.rs:180` `row` — 266 textual calls
- `crates/ui/src/artifacts/tab/comments_view.rs:626` `render_comment_row` — 1 textual calls
- `crates/ui/src/shell/work_outline_menu_ui.rs:563` `archive_selected_outline_row` — 1 textual calls
- `crates/ui/src/shell/work_outline_menu_ui.rs:664` `menu_row` — 69 textual calls
- `crates/ui/src/shell/work_outline_menu_ui.rs:886` `row` — 266 textual calls
- `crates/ui/src/shell/work_outline_menu_ui.rs:987` `right_click_selects_the_row_and_opens_the_item_menu` — 0 textual calls
- `crates/ui/src/shell/work_outline_menu_ui.rs:1458` `click_then_shift_arrows_move_a_chat_row_in_the_shell` — 0 textual calls
- `crates/ui/src/shell/work_outline_menu_ui.rs:1511` `click_row` — 8 textual calls
- `crates/ui/src/shell/work_outline_menu_ui.rs:1541` `archive_key_acts_on_the_selected_outline_row` — 0 textual calls
- `crates/ui/src/shell/work_outline_menu_ui.rs:1834` `space_row_menu_reorders_hides_renames_and_confirms_delete` — 0 textual calls
- `crates/ui/src/shell/spaces.rs:93` `row_drop_index` — 10 textual calls
- `crates/ui/src/shell/spaces.rs:1096` `sidebar_chat_row_shows_account_pill_only_for_managed_pins` — 0 textual calls
- `crates/ui/src/shell/spaces.rs:1747` `sidebar_hit_testing_uses_compact_and_mixed_row_heights` — 0 textual calls
- `crates/ui/src/shell/spaces.rs:2390` `render_sidebar_gap_row` — 1 textual calls
- `crates/ui/src/shell/spaces.rs:3112` `activate_spaces_menu_row` — 3 textual calls
- `crates/ui/src/shell/spaces.rs:3199` `activate_sidebar_view_row` — 4 textual calls
- `crates/ui/src/shell/chat_item_tests.rs:903` `the_plan_row_shows_the_changed_marker_until_the_plan_is_viewed` — 0 textual calls
- `crates/ui/src/shell/chat_item_tests.rs:960` `row_top` — 1 textual calls
- `crates/ui/src/shell/chat_item_tests.rs:1338` `a_list_row_fills_title_and_state_without_blanking_known_checks` — 0 textual calls
- `crates/ui/src/shell/running_ui.rs:192` `selected_running_row` — 1 textual calls
- `crates/ui/src/shell/running_ui.rs:1035` `render_running_row` — 1 textual calls
- `crates/ui/src/shell/running_ui.rs:1448` `an_admin_chat_row_shows_the_admin_marker_and_others_do_not` — 0 textual calls
- `crates/ui/src/shell/team_ui.rs:306` `render_worker_row` — 2 textual calls
- `crates/ui/src/shell/chat_item_ui.rs:1937` `pr_link_row` — 2 textual calls
- `crates/ui/src/shell/chat_item_ui.rs:2054` `ticket_link_row` — 2 textual calls
- `crates/ui/src/shell/work_outline_ui.rs:2650` `selected_row_chat` — 6 textual calls
- `crates/ui/src/shell/work_outline_ui.rs:2961` `enter_and_down_from_search_move_to_the_first_row` — 0 textual calls
- `crates/ui/src/shell/pr_review_ui.rs:1393` `clicking_review_on_an_authored_row_starts_one_review` — 0 textual calls
- `crates/ui/src/shell/pr_review_ui.rs:1415` `an_authored_row_with_a_review_chat_offers_open_review_the_menu_and_fix_ci` — 0 textual calls
- `crates/ui/src/shell/pr_review_ui.rs:1463` `enter_on_an_authored_row_opens_the_review_chat_or_starts_a_review` — 0 textual calls
- `crates/ui/src/shell/perf_tests.rs:28` `chat_row` — 8 textual calls
- `crates/ui/src/shell/workspace_search.rs:845` `render_workspace_search_row` — 0 textual calls
- `crates/ui/src/shell/admin_tests.rs:288` `a_compact_row_menu_offers_grant_then_renew_and_revoke` — 0 textual calls
- `crates/ui/src/shell/admin_tests.rs:306` `a_compact_row_chat_without_a_space_gets_no_admin_entries` — 0 textual calls
- `crates/ui/src/changes/ref_picker.rs:143` `choose_ref_row` — 2 textual calls
- `crates/ui/src/changes/tests/ref_targets.rs:324` `choosing_a_row_binds_it_through_the_engine` — 0 textual calls
- `crates/ui/src/markdown/render.rs:448` `invalidate_row` — 1 textual calls
- `crates/ui/src/markdown/render.rs:1866` `find_painted_in_row` — 4 textual calls
- `crates/ui/src/markdown/veil.rs:352` `seeded_row_adopts_existing_text_without_fading` — 0 textual calls
- `crates/ui/src/markdown/veil.rs:371` `default_row_veil_fades_first_text` — 0 textual calls
- `crates/ui/src/markdown/veil.rs:497` `row_veil_tracks_elements_independently` — 0 textual calls
- `crates/ui/src/linear/project_picker.rs:356` `render_row` — 24 textual calls
- `crates/ui/src/linear/panel.rs:1331` `row_focus_key` — 4 textual calls
- `crates/ui/src/linear/panel.rs:1336` `sync_row_focus` — 1 textual calls
- `crates/ui/src/linear/panel.rs:1363` `row_focus_handle` — 1 textual calls
- `crates/ui/src/linear/panel.rs:1373` `render_issue_row` — 2 textual calls
- `crates/ui/src/terminal/emulator.rs:396` `row_text` — 22 textual calls
- `crates/ui/src/terminal/emulator.rs:429` `plain_text_lands_on_row_zero` — 0 textual calls
- `crates/ui/src/terminal/emulator.rs:689` `line_selection_takes_the_whole_row` — 0 textual calls
- `crates/ui/src/terminal/view.rs:633` `shape_row` — 1 textual calls
- `crates/ui/src/work_outline/templates_tests.rs:603` `a_linear_project_row_creates_a_project_item_from_a_template_and_links_it` — 0 textual calls
- `crates/ui/src/work_outline/required_actions.rs:329` `row_for` — 1 textual calls
- `crates/ui/src/work_outline/context_menu.rs:949` `row_anchor` — 1 textual calls
- `crates/ui/src/work_outline/context_menu.rs:1014` `row_listener` — 1 textual calls
- `crates/ui/src/work_outline/context_menu.rs:1137` `space_row_menu_adds_reorder_hide_rename_and_delete` — 0 textual calls
- `crates/ui/src/work_outline/linear_tests.rs:591` `narrow_widths_drop_the_dot_then_the_assignee` — 0 textual calls
- `crates/ui/src/work_outline/linear_tests.rs:1466` `linear_row_shows_the_identifier_in_the_ticket_link` — 0 textual calls
- `crates/ui/src/work_outline/execution.rs:130` `owns_row` — 4 textual calls
- `crates/ui/src/work_outline/execution.rs:915` `row` — 266 textual calls
- `crates/ui/src/work_outline/pr_sync_outline.rs:79` `row_for_test` — 3 textual calls
- `crates/ui/src/work_outline/pr_sync_outline.rs:87` `measured_row_width_for_test` — 1 textual calls
- `crates/ui/src/work_outline/restructure_tests.rs:568` `row_selector` — 2 textual calls
- `crates/ui/src/work_outline/restructure_tests.rs:903` `click_row` — 8 textual calls
- `crates/ui/src/work_outline/restructure_tests.rs:940` `f2_and_r_rename_a_chat_row` — 0 textual calls
- `crates/ui/src/work_outline/restructure_tests.rs:974` `clicking_a_row_focuses_the_outline_for_f2_and_r` — 0 textual calls
- `crates/ui/src/work_outline/outline_view.rs:382` `row_kind_name` — 2 textual calls
- `crates/ui/src/work_outline/outline_view.rs:896` `selected_row` — 21 textual calls
- `crates/ui/src/work_outline/outline_view.rs:982` `focus_first_row` — 1 textual calls
- `crates/ui/src/work_outline/outline_view.rs:2524` `row_height` — 7 textual calls
- `crates/ui/src/work_outline/outline_view.rs:2662` `render_editor_row` — 1 textual calls
- `crates/ui/src/work_outline/outline_view.rs:2824` `render_row` — 24 textual calls
- `crates/ui/src/work_outline/outline_view.rs:4269` `minimal_outline_row_heights_match_existing_values` — 0 textual calls
- `crates/ui/src/work_outline/required_actions_view.rs:1025` `render_row` — 24 textual calls
- `crates/ui/src/work_outline/revision_diff.rs:96` `line_row` — 1 textual calls
- `crates/ui/src/work_outline/details_template_review.rs:377` `render_review_row` — 1 textual calls
- `crates/ui/src/work_outline/restructure.rs:509` `chat_moves_follow_visual_row_order_across_depth` — 0 textual calls
- `crates/ui/src/work_outline/restructure.rs:607` `drop_zones_split_the_row_into_edge_bands` — 0 textual calls
- `crates/ui/src/work_outline/linear_outline.rs:306` `issue_row_tooltip` — 5 textual calls
- `crates/ui/src/work_outline/linear_outline.rs:324` `issue_row_icons` — 2 textual calls
- `crates/ui/src/work_outline/linear_outline.rs:514` `issue_row_slots` — 1 textual calls
- `crates/ui/src/work_outline/linear_outline.rs:793` `narrow_rows_drop_the_dot_then_the_assignee_and_never_needs_or_status` — 0 textual calls
- `crates/ui/src/work_outline/compact_rows_tests.rs:105` `a_single_chat_item_renders_one_row_with_the_chat_state` — 0 textual calls
- `crates/ui/src/work_outline/compact_rows_tests.rs:273` `keyboard_navigation_treats_the_compact_row_as_one_row` — 1 textual calls
- `crates/ui/src/work_outline/compact_rows_tests.rs:323` `dragging_a_compact_row_moves_the_item` — 0 textual calls
- `crates/ui/src/work_outline/compact_rows_tests.rs:482` `status_glyph_click_opens_details_and_the_row_still_opens_the_chat` — 0 textual calls
- `crates/ui/src/work_outline/tree_guides.rs:287` `row` — 266 textual calls
- `crates/ui/src/work_outline/pr_sync_tests.rs:1538` `an_assigned_chat_marks_the_review_item_row` — 0 textual calls
- `crates/ui/src/work_outline/pr_sync_tests.rs:1551` `only_manual_and_launcher_links_mark_a_pr_row` — 0 textual calls
- `crates/ui/src/work_outline/model.rs:267` `row` — 266 textual calls
- `crates/ui/src/work_outline/model.rs:1559` `a_lone_live_chat_is_folded_into_its_item_row` — 0 textual calls
- `crates/ui/src/work_outline/model.rs:1601` `a_dangling_lone_chat_keeps_its_row` — 0 textual calls
- `crates/ui/src/work_outline/model.rs:1623` `a_second_chat_expands_the_row_and_selection_stays_on_the_item` — 0 textual calls
- `crates/ui/src/work_outline/model.rs:1646` `a_child_item_or_a_sub_chat_expands_the_row` — 0 textual calls
- `crates/ui/src/work_outline/model.rs:1675` `an_archived_chat_keeps_its_own_row` — 0 textual calls
- `crates/ui/src/work_outline/model.rs:1709` `keyboard_navigation_treats_the_compact_row_as_one_row` — 1 textual calls
- `crates/ui/src/work_outline/outline_view/tree_guides_view.rs:43` `row_title` — 34 textual calls
- `crates/ui/src/popover/choice_menu.rs:200` `the_cursor_starts_on_the_checked_row` — 0 textual calls
- `crates/ui/src/github/row_icons.rs:435` `pr_row_slots` — 1 textual calls
- `crates/ui/src/github/panel.rs:592` `row_sync` — 1 textual calls
- `crates/ui/src/github/panel.rs:687` `row_menu_trigger` — 1 textual calls
- `crates/ui/src/github/panel.rs:741` `render_row` — 24 textual calls
- `crates/ui/src/github/panel.rs:855` `row_key` — 2 textual calls
- `crates/ui/src/github/panel/menus.rs:244` `row_menu_rows` — 1 textual calls
- `crates/ui/src/github/panel/menus.rs:353` `pick_menu_row` — 2 textual calls
- `crates/ui/src/chat_type_view/pr_tabs.rs:214` `activate_dash_row` — 2 textual calls
- `crates/ui/src/chat_type_view/pr_tabs.rs:617` `render_dash_row` — 1 textual calls
- `crates/ui/src/files/markdown_preview.rs:1031` `render_row` — 24 textual calls
- `crates/ui/src/files/tree.rs:207` `render_tree_row` — 0 textual calls
- `crates/ui/src/files/tree.rs:492` `status_row` — 2 textual calls
- `crates/ui/src/files/mod.rs:590` `editor_context_menu_row` — 4 textual calls
- `crates/ui/src/files/search.rs:598` `render_search_row` — 0 textual calls
- `crates/ui/src/pickers/approval.rs:110` `effective_flag_prefers_the_row_then_the_carried_draft` — 0 textual calls
- `crates/ui/src/pr_page/mod.rs:551` `row` — 266 textual calls
- `crates/ui/src/menu_bar/tests.rs:283` `clicking_a_chat_row_opens_that_chat` — 0 textual calls
- `crates/ui/src/menu_bar/tests.rs:607` `an_admin_chat_row_carries_an_admin_tag_and_others_do_not` — 0 textual calls
- `crates/ui/src/perf/overview_bench.rs:15` `chat_row` — 8 textual calls

### Status indicator

- `crates/ui/src/context_usage.rs:185` `indicator_needs_a_reported_window` — 0 textual calls
- `crates/ui/src/status_style.rs:96` `indicator` — 5 textual calls
- `crates/ui/src/status_style.rs:115` `indicator_for_theme` — 19 textual calls
- `crates/ui/src/status_style.rs:123` `dot_color` — 5 textual calls
- `crates/ui/src/status_style.rs:132` `animated_indicator` — 5 textual calls
- `crates/ui/src/status_style.rs:164` `industrial_dot_uses_signal_for_working_and_neutral_for_idle` — 0 textual calls
- `crates/ui/src/pickers.rs:4151` `execution_branch_indicator` — 3 textual calls
- `crates/ui/src/pickers.rs:6599` `bound_chat_branch_indicator_names_and_cycles_member_repositories` — 0 textual calls
- `crates/ui/src/surface_chrome.rs:41` `active_key_dot` — 7 textual calls
- `crates/ui/src/state.rs:1927` `indicator_for` — 17 textual calls
- `crates/ui/src/state.rs:5451` `working_indicator_staleness` — 0 textual calls
- `crates/ui/src/state.rs:5468` `indicator_kinds` — 0 textual calls
- `crates/ui/src/artifacts/questions_ui.rs:154` `changed_dot` — 4 textual calls
- `crates/ui/src/shell/spaces.rs:2193` `status_dot_color` — 1 textual calls
- `crates/ui/src/work_outline/chips.rs:64` `attention_indicator` — 4 textual calls
- `crates/ui/src/work_outline/tree_guides_tests.rs:165` `drop_indicator_starts_at_destination_parent_guide` — 0 textual calls
- `crates/ui/src/github/row_icons.rs:92` `ci_dot` — 6 textual calls
- `crates/ui/src/github/row_icons.rs:725` `ci_dot_for_every_state` — 0 textual calls

### Readout


### Message and composer shells

- `crates/ui/src/transcript.rs:7497` `user_bubble_text` — 1 textual calls
- `crates/ui/src/transcript.rs:10316` `queued_turn_waits_for_its_bubble_without_replacing_the_live_turn` — 0 textual calls
- `crates/ui/src/theme.rs:1742` `user_bubble_bg` — 1 textual calls



## Style-specific references

- `crates/ui/src/appearance.rs:159`: `crate::ui_style::SurfaceModel::ThemeDefault => surface,`
- `crates/ui/src/appearance.rs:160`: `crate::ui_style::SurfaceModel::Opaque => SurfacePreference::Opaque,`
- `crates/ui/src/appearance.rs:161`: `crate::ui_style::SurfaceModel::Glass => SurfacePreference::Frosted,`
- `crates/ui/src/artifacts/tab.rs:1062`: `.when(theme.ui_style.id == crate::ui_style::UiStyleId::Area, |el| el.rounded_full().border_1().border_color(theme.border_strong))`
- `crates/ui/src/artifacts/tab.rs:1098`: `.when(theme.ui_style.id == crate::ui_style::UiStyleId::Area, |toolbar| {`
- `crates/ui/src/artifacts/tab.rs:1199`: `if theme.ui_style.id != crate::ui_style::UiStyleId::Area { return None; }`
- `crates/ui/src/changes/explanation.rs:451`: `theme.ui_style.controls == crate::ui_style::ControlVariant::Key,`
- `crates/ui/src/changes/explanation.rs:491`: `== crate::ui_style::StatusIndicator::Dot`
- `crates/ui/src/changes/walkthrough.rs:748`: `theme.ui_style.controls == crate::ui_style::ControlVariant::Key,`
- `crates/ui/src/changes/walkthrough.rs:831`: `let industrial = theme.ui_style.controls == crate::ui_style::ControlVariant::Key;`
- `crates/ui/src/changes/walkthrough.rs:1030`: `== crate::ui_style::StatusIndicator::Dot`
- `crates/ui/src/changes/walkthrough.rs:1077`: `.when(theme.ui_style.controls == crate::ui_style::ControlVariant::Key, |el| el.self_start().flex().items_center().gap(px(8.0)).px(px(16.0)).py(px(8.0)))`
- `crates/ui/src/changes/walkthrough.rs:1079`: `.when(!selected_reviewed && theme.ui_style.controls == crate::ui_style::ControlVariant::Key, |el| { let foreground = if crate::theme::contrast_ratio(theme.text, theme.success) >= crate::theme::contrast_ratio(theme.bg, theme.success) { theme.text } else { theme.bg }; el.bg(theme.success).text_color(foreground) })`
- `crates/ui/src/changes/walkthrough.rs:1110`: `.when(self.files.is_empty() && theme.ui_style.controls == crate::ui_style::ControlVariant::Key, |el| el.child(`
- `crates/ui/src/changes/walkthrough.rs:1124`: `.when(theme.ui_style.controls == crate::ui_style::ControlVariant::Key, |el| el.child(crate::surface_chrome::grille(&theme))),`
- `crates/ui/src/changes/walkthrough.rs:1181`: `.when(theme.ui_style.controls == crate::ui_style::ControlVariant::Key, |el| el.font_family(theme.font_mono.clone()))`
- `crates/ui/src/changes/walkthrough.rs:1182`: `.child(if theme.ui_style.controls == crate::ui_style::ControlVariant::Key { crate::surface_chrome::display(&theme).child(progress).into_any_element() } else { div().child(progress).into_any_element() })`
- `crates/ui/src/changes/walkthrough.rs:1188`: `theme.ui_style.controls == crate::ui_style::ControlVariant::Key,`
- `crates/ui/src/changes/walkthrough.rs:1223`: `theme.ui_style.surface == crate::ui_style::SurfaceModel::Opaque && theme.ui_style.id != crate::ui_style::UiStyleId::Area,`
- `crates/ui/src/changes/walkthrough.rs:1255`: `== crate::ui_style::SurfaceModel::Opaque,`
- `crates/ui/src/composer.rs:7989`: `.when(!theme.is_frost() && theme.ui_style.surface == crate::ui_style::SurfaceModel::ThemeDefault, |el| el.shadow_lg())`
- `crates/ui/src/composer.rs:8105`: `crate::ui_style::ConversationVariant::Area => (true, false),`
- `crates/ui/src/composer.rs:8106`: `crate::ui_style::ConversationVariant::Industrial => (false, true),`
- `crates/ui/src/composer.rs:8107`: `crate::ui_style::ConversationVariant::Standard => (false, false),`
- `crates/ui/src/composer.rs:8200`: `let area = matches!(theme.ui_style.conversation, crate::ui_style::ConversationVariant::Area);`
- `crates/ui/src/composer.rs:8693`: `.when(theme.ui_style.controls == crate::ui_style::ControlVariant::Key, |el| crate::surface_chrome::icon_key(el, &theme, false))`
- `crates/ui/src/composer.rs:8742`: `.when(!theme.is_frost() && theme.ui_style.surface == crate::ui_style::SurfaceModel::ThemeDefault, |el| el.shadow_lg());`
- `crates/ui/src/composer.rs:8803`: `.when(theme.ui_style.conversation == crate::ui_style::ConversationVariant::Industrial, |el| el.child(`
- `crates/ui/src/composer.rs:8847`: `crate::ui_style::ConversationVariant::Area => {`
- `crates/ui/src/composer.rs:8858`: `crate::ui_style::ConversationVariant::Standard | crate::ui_style::ConversationVariant::Industrial => if expanded {`
- `crates/ui/src/context_usage.rs:15`: `let industrial = theme.ui_style.controls == crate::ui_style::ControlVariant::Key;`
- `crates/ui/src/files/mod.rs:50`: `.when(theme.ui_style.controls == crate::ui_style::ControlVariant::Key, |element| element.size(px(theme.ui_style.tokens.control_size)))`
- `crates/ui/src/markdown/render.rs:2458`: `if theme.ui_style.id == crate::ui_style::UiStyleId::Area {`
- `crates/ui/src/markdown/render.rs:2585`: `.when(!copied && theme.ui_style.id == crate::ui_style::UiStyleId::Area, |el| el.child("Copy"))`
- `crates/ui/src/markdown/render.rs:2598`: `if theme.ui_style.id == crate::ui_style::UiStyleId::Area {`
- `crates/ui/src/markdown/render.rs:2654`: `.when(theme.ui_style.id == crate::ui_style::UiStyleId::Area, |el| el.rounded(px(14.0)).bg(theme.bg).border_color(theme.bg).p(px(4.0)))`
- `crates/ui/src/markdown/render.rs:2975`: `if theme.ui_style.id == crate::ui_style::UiStyleId::Area {`
- `crates/ui/src/overview.rs:3271`: `let indicator = if theme.ui_style.status_indicator == crate::ui_style::StatusIndicator::Dot {`
- `crates/ui/src/overview.rs:5522`: `let indicator = if theme.ui_style.status_indicator == crate::ui_style::StatusIndicator::Dot {`
- `crates/ui/src/popover.rs:335`: `&& theme.ui_style.surface == crate::ui_style::SurfaceModel::ThemeDefault,`
- `crates/ui/src/popover.rs:849`: `if theme.ui_style.controls == crate::ui_style::ControlVariant::Key {`
- `crates/ui/src/popover.rs:1109`: `.when(theme.ui_style.controls != crate::ui_style::ControlVariant::Minimal, |el| el.bg(theme.input_bg).border_1().border_color(theme.border))`
- `crates/ui/src/popover.rs:1143`: `.when(!theme.is_frost() && theme.ui_style.surface == crate::ui_style::SurfaceModel::ThemeDefault, |el| el.shadow_lg())`
- `crates/ui/src/popover.rs:1178`: `.when(theme.ui_style.controls != crate::ui_style::ControlVariant::Minimal, |el| el.bg(theme.input_bg).border_color(theme.border))`
- `crates/ui/src/popover.rs:1214`: `.when(theme.ui_style.controls == crate::ui_style::ControlVariant::Key, |el| el.rounded(px(4.0)).bg(theme.accent).border_1().border_color(theme.border).shadow(vec![gpui::BoxShadow { color: crate::surface_chrome::key_ledge(theme), offset: gpui::point(px(0.0), px(2.0)), blur_radius: px(0.0), spread_radius: px(0.0), inset: false }]))`
- `crates/ui/src/popover.rs:1217`: `.text_color(if theme.ui_style.controls == crate::ui_style::ControlVariant::Key { theme.on_accent } else { theme.on_solid })`
- `crates/ui/src/settings/widgets.rs:416`: `if theme.ui_style.controls == crate::ui_style::ControlVariant::Key {`
- `crates/ui/src/settings/widgets.rs:492`: `theme.ui_style.controls == crate::ui_style::ControlVariant::Key,`
- `crates/ui/src/shell/chat_item_ui.rs:295`: `.when(theme.ui_style.surface == crate::ui_style::SurfaceModel::Opaque, |el| el.bg(theme.surface).border_color(theme.border).px(px(16.0)))`
- `crates/ui/src/shell/chat_item_ui.rs:501`: `.when(theme.ui_style.surface == crate::ui_style::SurfaceModel::Opaque, |el| el.bg(theme.surface_raised))`
- `crates/ui/src/shell/chat_item_ui.rs:504`: `.when(is_model && theme.ui_style.controls == crate::ui_style::ControlVariant::Key && tone == ChipTone::Plain, |chip| {`
- `crates/ui/src/shell/chat_item_ui.rs:520`: `if is_model && theme.ui_style.controls == crate::ui_style::ControlVariant::Key && tone == ChipTone::Plain {`
- `crates/ui/src/shell/chat_item_ui.rs:632`: `.when(theme.ui_style.surface == crate::ui_style::SurfaceModel::Opaque, |el| el.bg(theme.surface_raised))`
- `crates/ui/src/shell/chat_item_ui.rs:673`: `.when(theme.ui_style.surface == crate::ui_style::SurfaceModel::ThemeDefault, |el| el.shadow_md())`
- `crates/ui/src/shell/perf_tests.rs:63`: `crate::ui_style::init(crate::ui_style::UiStyleId::Industrial, cx);`
- `crates/ui/src/shell/running_ui.rs:1043`: `let indicator = if theme.ui_style.status_indicator == crate::ui_style::StatusIndicator::Dot {`
- `crates/ui/src/shell/spaces.rs:2067`: `div().h(px(if theme.ui_style.id == crate::ui_style::UiStyleId::Area { 0.0 } else { theme.ui_style.tokens.rule_width.unwrap_or(1.0) })).bg(if theme.ui_style.tokens.rule_width.is_some() { theme.border } else { theme.border.opacity(0.6) })`
- `crates/ui/src/shell/spaces.rs:2071`: `let key = theme.ui_style.controls == crate::ui_style::ControlVariant::Key || theme.ui_style.id == crate::ui_style::UiStyleId::Area;`
- `crates/ui/src/shell/team_ui.rs:326`: `let indicator = if theme.ui_style.status_indicator == crate::ui_style::StatusIndicator::Dot {`
- `crates/ui/src/shell.rs:7118`: `.when(theme.ui_style.surface == crate::ui_style::SurfaceModel::Opaque, |el| el.bg(theme.surface))`
- `crates/ui/src/shell.rs:7119`: `.when(theme.ui_style.surface == crate::ui_style::SurfaceModel::Opaque && theme.ui_style.id != crate::ui_style::UiStyleId::Area, |el| el.border_r_1().border_color(theme.border))`
- `crates/ui/src/shell.rs:7120`: `.child(if theme.ui_style.id == crate::ui_style::UiStyleId::Area {`
- `crates/ui/src/shell.rs:7299`: `if theme.ui_style.id == crate::ui_style::UiStyleId::Area && search_query.is_none() {`
- `crates/ui/src/shell.rs:7387`: `let glyph = if theme.ui_style.status_indicator == crate::ui_style::StatusIndicator::Dot {`
- `crates/ui/src/shell.rs:7508`: `let glyph: AnyElement = if theme.ui_style.status_indicator == crate::ui_style::StatusIndicator::Dot {`
- `crates/ui/src/shell.rs:7549`: `.when(theme.ui_style.status_indicator == crate::ui_style::StatusIndicator::Dot, |el| el.flex().items_center().gap(px(8.0)).child(crate::status_style::indicator_for_theme(crate::status_style::Status::Idle, &theme)))`
- `crates/ui/src/shell.rs:7550`: `.when(theme.ui_style.controls == crate::ui_style::ControlVariant::Key, |el| el.font_family(theme.font_mono.clone()))`
- `crates/ui/src/shell.rs:7602`: `let area = theme.ui_style.id == crate::ui_style::UiStyleId::Area;`
- `crates/ui/src/shell.rs:7605`: `let selected_wash = if area { identity } else if theme.ui_style.surface == crate::ui_style::SurfaceModel::Opaque { theme.element_active } else { crate::theme::glass_selected_bg() };`
- `crates/ui/src/shell.rs:7841`: `.when(theme.ui_style.controls == crate::ui_style::ControlVariant::Key, |el| el.font_family(theme.font_mono.clone()))`
- `crates/ui/src/shell.rs:8462`: `.when(theme.ui_style.controls == crate::ui_style::ControlVariant::Key, |el| el.child(`
- `crates/ui/src/shell.rs:10192`: `.when(theme.ui_style.controls == crate::ui_style::ControlVariant::Key, |el| el.child(`
- `crates/ui/src/shell.rs:12754`: `let main = if theme.ui_style.controls == crate::ui_style::ControlVariant::Key {`
- `crates/ui/src/shell.rs:12764`: `.when(theme.ui_style.surface == crate::ui_style::SurfaceModel::Opaque, |el| el.bg(theme.bg))`
- `crates/ui/src/shell.rs:12765`: `.when(theme.ui_style.controls == crate::ui_style::ControlVariant::Key, |el| el.p(px(8.0)).rounded(px(12.0)).bg(theme.surface))`
- `crates/ui/src/shell.rs:12833`: `.when(theme.ui_style.surface == crate::ui_style::SurfaceModel::Opaque, |el| el.bg(theme.surface))`
- `crates/ui/src/shell.rs:12873`: `.when(theme.ui_style.surface == crate::ui_style::SurfaceModel::Opaque, |el| el.bg(theme.surface))`
- `crates/ui/src/shell.rs:12874`: `.when(theme.ui_style.surface == crate::ui_style::SurfaceModel::Opaque && theme.ui_style.id != crate::ui_style::UiStyleId::Area, |el| el.border_b_1().border_color(theme.border))`
- `crates/ui/src/status_style.rs:116`: `if theme.ui_style.status_indicator == crate::ui_style::StatusIndicator::Dot {`
- `crates/ui/src/status_style.rs:124`: `if theme.ui_style.status_indicator == crate::ui_style::StatusIndicator::Dot {`
- `crates/ui/src/status_style.rs:140`: `&& theme.ui_style.status_indicator == crate::ui_style::StatusIndicator::Dot`
- `crates/ui/src/surface_chrome.rs:20`: `if theme.ui_style.controls == crate::ui_style::ControlVariant::Key {`
- `crates/ui/src/surface_chrome.rs:32`: `if theme.ui_style.controls == crate::ui_style::ControlVariant::Key {`
- `crates/ui/src/surface_chrome.rs:59`: `if theme.ui_style.controls == crate::ui_style::ControlVariant::Key {`
- `crates/ui/src/surface_chrome.rs:66`: `(active && theme.ui_style.controls == crate::ui_style::ControlVariant::Key)`
- `crates/ui/src/surface_chrome.rs:79`: `.when(theme.ui_style.controls != crate::ui_style::ControlVariant::Minimal, |el| el.bg(theme.input_bg).border_1().border_color(theme.border))`
- `crates/ui/src/transcript.rs:6212`: `let area = matches!(conversation, crate::ui_style::ConversationVariant::Area);`
- `crates/ui/src/transcript.rs:6334`: `if matches!(conversation, crate::ui_style::ConversationVariant::Area) {`
- `crates/ui/src/transcript.rs:6344`: `.when(matches!(conversation, crate::ui_style::ConversationVariant::Industrial), |el| el.bg(theme.surface).py(px(8.0)).border_1().border_color(theme.border))`
- `crates/ui/src/transcript.rs:6345`: `.when(matches!(conversation, crate::ui_style::ConversationVariant::Area), |el| el.bg(identity).max_w_full().w_full())`
- `crates/ui/src/transcript.rs:6351`: `crate::ui_style::ConversationVariant::Industrial => div().min_w_0().max_w(px(self.content_width * 0.8)).flex().flex_col().items_end().gap(px(4.0))`
- `crates/ui/src/transcript.rs:6354`: `crate::ui_style::ConversationVariant::Standard | crate::ui_style::ConversationVariant::Area => bubble.into_any_element(),`
- `crates/ui/src/transcript.rs:6573`: `let inner = if matches!(conversation, crate::ui_style::ConversationVariant::Industrial)`
- `crates/ui/src/work_outline/compact_rows_tests.rs:95`: `crate::ui_style::set_style(crate::ui_style::UiStyleId::Industrial, window, cx);`
- `crates/ui/src/work_outline/details_panel.rs:1325`: `.border_color(if theme.ui_style.controls == crate::ui_style::ControlVariant::Key { theme.border } else { theme.accent })`
- `crates/ui/src/work_outline/details_panel.rs:1326`: `.bg(if theme.ui_style.controls == crate::ui_style::ControlVariant::Key { theme.input_bg } else { theme.surface })`
- `crates/ui/src/work_outline/details_panel.rs:1346`: `.when(theme.ui_style.controls == crate::ui_style::ControlVariant::Key, |el| el.border_l_2().border_color(theme.industrial_colors().rule))`
- `crates/ui/src/work_outline/details_panel.rs:1496`: `.when(theme.ui_style.controls == crate::ui_style::ControlVariant::Key, |el| el.child(`
- `crates/ui/src/work_outline/details_panel.rs:1548`: `.when(theme.ui_style.controls == crate::ui_style::ControlVariant::Key, |el| el.flex().items_center().gap(px(8.0)))`
- `crates/ui/src/work_outline/details_panel.rs:1550`: `.when(active && status == WorkItemStatus::Completed && theme.ui_style.controls == crate::ui_style::ControlVariant::Key, |el| {`
- `crates/ui/src/work_outline/details_panel.rs:1586`: `.when(theme.ui_style.controls == crate::ui_style::ControlVariant::Key, |el| el.child(`
- `crates/ui/src/work_outline/details_panel.rs:1598`: `.when(theme.ui_style.surface == crate::ui_style::SurfaceModel::Opaque, |el| el.bg(theme.surface))`
- `crates/ui/src/work_outline/details_panel.rs:1835`: `.when(theme.ui_style.controls == crate::ui_style::ControlVariant::Key, |el| el.font_family(theme.font_mono.clone()))`
- `crates/ui/src/work_outline/details_panel.rs:2034`: `div().when(theme.ui_style.controls == crate::ui_style::ControlVariant::Key, |el| el.flex().items_start()).child(`
- `crates/ui/src/work_outline/outline_view.rs:3565`: `.when(theme.ui_style.controls == crate::ui_style::ControlVariant::Key, |el| {`
- `crates/ui/src/work_outline/outline_view.rs:4100`: `.when(theme.ui_style.surface == crate::ui_style::SurfaceModel::Opaque, |el| el.bg(theme.surface))`
- `crates/ui/src/work_outline/resizable_panes_tests.rs:76`: `crate::ui_style::set_style(crate::ui_style::UiStyleId::Industrial, window, cx);`

## Candidate screen-owned migration slices

Assign each slice only the paths listed; shared components are extended by the
foundation owner before concurrent screen migrations begin.

| Slice | Owned paths |
| --- | --- |
| Shell and sidebar | `shell.rs`, `shell/`, `rail.rs` |
| Outline and item details | `work_outline/`, `overview.rs`, `overview_grouping.rs`, `overview_positions.rs` |
| Conversation and composer | `transcript.rs`, `composer.rs`, `composer/`, `composer_dock.rs`, `markdown/` |
| Changes and walkthrough | `changes.rs`, `changes/`, `files/` |
| Settings and appearance | `settings.rs`, `settings/`, `appearance.rs` |
| Browser and artifacts | `browser/`, `artifacts/` |
| Linear and Team | `linear/`, `team/`, `team.rs` |
| Menus and remaining surfaces | `popover.rs`, `pickers.rs`, `app_menus.rs`, `menu_bar.rs`, other paths not owned above |

### Changes and walkthrough audit (in progress)

Locations refer to `91995af39`. This index identifies the existing appearance
decisions and the surfaces requiring manual migration; it is not yet a complete
inventory of inline controls.

| Surface | Candidate locations |
| --- | --- |
| Walkthrough navigation, notes, progress and controls | `changes/walkthrough.rs:748`, `831`, `866`, `905`, `913`, `952`, `1029`, `1077`, `1110`, `1124`, `1144`, `1181`, `1188`, `1223`, `1254` |
| Explanation trigger, answer and loading indicator | `changes/explanation.rs:437`, `448`, `451`, `490` |
| Changes toolbar, selectors and menus | `changes.rs:5001`, `5012`, `5097`, `5382`, `5413`, `5547`, `5632`, `5698`, `5903`, `6012`; `changes/ref_picker.rs:188` |
| Changes file and diff rows | `changes/file_list.rs:445`; `changes.rs:4305`, `4665`, `4793`, `4935`, `6193`, `6208`, `6283`, `6423`, `6570`, `6626` |
| File explorer toolbar, tree and search | `files/mod.rs:45`, `268`, `590`, `1042`; `files/tree.rs:176`, `207`, `492`; `files/search.rs:548`, `598` |
| File preview and review comments | `files/preview.rs:1960`, `2147`, `2156`, `2659`, `2770`, `2861`; `files/markdown_preview.rs:1031`, `1215`, `1358`, `1450` |

The explicit production style and layout decisions are the walkthrough and
explanation references above, `files/mod.rs:46–59`, and reveal behavior at
`files/markdown_preview.rs:1450` and `files/preview.rs:2721,2836`. Token reads
and indirect `surface_chrome` calls also require migration. Default diff code
geometry and data-specific status colors need separate review before deciding
which literals stay in the baseline.

At `e02f7bfa2`, the indexed direct style and layout references have moved into
`components/changes_parts.rs`; a direct search of these owned files for
`theme.ui_style`, `UiStyleId::`, and `UiLayout::` returns no matches. Remaining
literal `ui_rems` calls in production, pending the rest of this audit:

- `changes/explanation.rs` trigger and answer text: stays literal, in baseline:
  fixed explanatory text size across the current styles.
- `changes/walkthrough.rs` note body and root text: stays literal, in baseline:
  fixed review text and line geometry, including the note-height calculation.
- `changes/walkthrough.rs` highlights and questions labels: stays literal, in
  baseline: fixed secondary text size; label case is component-owned.

The remaining inline surfaces still require manual audit; this list is not a
claim that every fixed pixel measurement has been reviewed.

### Shell and sidebar audit (in progress)

Locations refer to `d68928981`. This is a starting index, not a complete
inventory of inline controls; the remaining shell submodules and every inline
control still need manual review before the audit is complete.

| Surface | Candidate locations |
| --- | --- |
| Titlebar, window controls and navigation | `shell.rs:6491`, `shell.rs:6609`, `shell.rs:6773`, `shell.rs:6892`, `shell.rs:11928`, `shell.rs:11992`, `shell.rs:12038`, `shell.rs:12083` |
| Sidebar and chat rows | `shell.rs:7098`, `shell.rs:7263`, `shell.rs:7899`, `shell.rs:8000`, `shell.rs:8049`; `shell/spaces.rs:2066`, `shell/spaces.rs:2070`, `shell/spaces.rs:4330`, `shell/spaces.rs:4856`, `shell/spaces.rs:4931`, `shell/spaces.rs:5011` |
| Chat header and side panel | `shell/chat_item_ui.rs:66`, `shell/chat_item_ui.rs:245`, `shell/chat_item_ui.rs:338`, `shell/chat_item_ui.rs:401`, `shell/chat_item_ui.rs:469`, `shell/chat_item_ui.rs:651`, `shell/chat_item_ui.rs:704`, `shell/chat_item_ui.rs:754` |
| Tabs and header buttons | `shell.rs:10960`, `shell.rs:12112`, `shell/tabs.rs:226`, `shell/tabs.rs:575` |
| Team and running controls | `shell/team_ui.rs:83`, `shell/team_ui.rs:148`, `shell/team_ui.rs:276`, `shell/team_ui.rs:306`, `shell/team_ui.rs:729`; `shell/running_ui.rs:540`, `shell/running_ui.rs:575`, `shell/running_ui.rs:1035`, `shell/running_ui.rs:1197`, `shell/running_ui.rs:1207` |
| Other controls requiring review | `shell/start_chat_ui.rs:1057`, `shell/start_chat_ui.rs:1080`, `shell/wake_ui.rs:174`, `shell/actions_ui.rs:1060`, `shell/workspace_search.rs:845`, `rail.rs:419` |

Known production style/layout branches: `shell.rs:7118`, `7119`, `7120`,
`7299`, `7377`, `7387`, `7416`, `7423`, `7508`, `7549`, `7550`, `7602`,
`7605`, `7841`, `8462`, `10192`, `12754`, `12764`, `12765`, `12833`,
`12873`, `12874`; `shell/chat_item_ui.rs:295`, `501`, `504`, `520`, `632`,
`673`; `shell/spaces.rs:2067`, `2071`; `shell/team_ui.rs:326`;
`shell/running_ui.rs:1043`. Further review must include indirect style and
layout decisions and inline literals, not just these textual matches.

The titlebar's sidebar toggle and new-conversation action use
`window_control_button` (`shell.rs:11928`); back and forward use
`nav_history_button` (`shell.rs:12083`). Rename, expand, files and changes
use `header_icon_button` (`shell.rs:12112`, `shell/tabs.rs:393`, `426`,
`446`, `517`). Windows and Linux captions have separate button builders at
`shell.rs:11992` and `12038`. The title and target text are built inline at
`shell/tabs.rs:474` and `535`. The sidebar has an Area-only wordmark and
new-conversation button at `shell.rs:7122` and `7123`; settings navigation
rows and Back are inline at `shell.rs:7180` and `7234`.

Area chat rows take an early return at `shell.rs:7299`; this omits the normal
status, account and change-request elements built later in the function.
The normal row contains status glyph/word (`shell.rs:7387`, `7508`), jump
badge (`shell.rs:7435`), archive action (`shell.rs:7454`), selected fill
(`shell.rs:7605`), account badge (`shell.rs:7784`) and change-request badge
(`shell.rs:7810`). Preserve their behavior when consolidating the row.

The shell branch has since moved titlebar controls, sidebar frame and section
chrome, Area row, chat-header frame and chips, Item/Team tabs, right-surface
tab frame, activity indicators, account trigger and rail into
`components/shell_parts.rs`. Area rows now show send-status overrides, account
and pull-request badges, jump hints and an archive action. The separate Area
row still needs a complete comparison with the normal row for project, branch,
remote and other metadata. Remaining inline controls in the shell and its
submodules are not yet inventoried or migrated; this slice is not complete.

Additional Item/Team candidates on shell branch `731363824` (locations move as
the migration proceeds):

| Control | Location |
| --- | --- |
| Chat title, breadcrumb and breadcrumb buttons | `shell/chat_item_ui.rs:241`, `312`, `338` |
| Attention badges | `shell/chat_item_ui.rs:370` |
| Item accordion header and toggle | `shell/chat_item_ui.rs:736`, `742` |
| Item links, path segments and identifier buttons | `shell/chat_item_ui.rs:815`, `1711`, `1745`, `1850`, `1937` |
| Item team roll-up and open-Team action | `shell/team_ui.rs:218`, `236` |
| Team row status, unseen badge, activity, report toggle and jump action | `shell/team_ui.rs:328`, `343`, `382`, `404`, `417`, `645` |
| Team archive confirmation and show-archived toggle | `shell/team_ui.rs:690`, `773`, `816`, `842` |
| Wake toggle chip and held-event delivery action | `shell/wake_ui.rs:194`, `273` |

Each slice must audit its own inline call sites and remove screen-owned style
and layout branches after shared components support the existing treatments.
The candidate index is not a substitute for that audit.

## Outline and item details migration audit (partial)

Locations refer to foundation commit `d68928981`. This is a starting inventory,
not a completed screen-by-screen audit. All listed renderers still need their
inline elements checked against the shared components.

- `work_outline/outline_view.rs:2524,2587,2718,2776,2824,3365,3549,3597,3803,3848`: row geometry, first-use prompt, artifact/drift/proposal chips, row body, totals and status indicators. Style/layout branches occur at 2624, 3145, 3152, 3565, 3649, 3895 and 4100; style tokens also control row geometry at 2673 and 3211–3219. Tree guide geometry is in `work_outline/outline_view/tree_guides_view.rs:180`.
- `work_outline/details_panel.rs:1224`: collapsible section header applies tracked caps whenever a label-size token exists, including Area. Controls and layout branch at 1317–1318, 1338, 1380, 1394, 1488, 1540–1542, 1578, 1590, 1827 and 2026–2046; the title and status control are at 1509 and 1540. Check Archive in the same renderer.
- `work_outline/details_instructions.rs:441,757,958,1082,1138`: instruction, effective instruction, PR prompt and approval rule sections. `details_templates.rs:231,309,405`, `details_container_template.rs:509,649`, and `details_template_review.rs:283,377`: template controls, editor rows and review.
- `work_outline/details_user_actions.rs:456,502,585,678,707,899,1184`: required-action and proposal sections and rows. `required_actions_view.rs:1022,1052,1107,1127,1823`: ad-hoc buttons, links and action cards. `chips.rs:38,218` and `hints.rs:24,479`: chips, details buttons and shortcut badge. Check `context_menu.rs` and the other `details_*` modules for inline controls.
- `overview.rs:3271,5522`: status-indicator branches; 3390, 3450, 4534, 4884: buttons; 3481: item/chat panel content; 5303, 5449, 5494: lane header and list rows; 5831, 5988: outline toolbar and filter. Inspect `overview_grouping.rs` and `overview_positions.rs` for remaining presentation decisions.

### Remaining inline presentation index

Mechanical inventory at `4dc1911d`: each line contains an inline `div()`, radius, type-size, background, or border expression in the owned files. This deliberately includes structural containers, non-covered Overview content, and tests embedded in production files; each entry must be classified during migration. Components already called from these lines are not necessarily ad-hoc. The index includes the instruction, proposal, template, and Required Actions surfaces as well as outline rows. There are no production matches for direct `UiStyleId` / `UiLayout` comparisons or `ui_style` control, surface, layout, indicator, conversation, or behavior branches in the owned files at this commit.

- `crates/ui/src/overview.rs`: 692, 699, 700, 702, 1255, 1258, 1259, 1260, 1261, 1264, 1384, 1389, 1392, 1394, 1396, 1399, 1403, 1404, 3143, 3173, 3182, 3183, 3184, 3201, 3210, 3211, 3212, 3285, 3298, 3299, 3302, 3303, 3305, 3306, 3315, 3319, 3328, 3333, 3340, 3346, 3347, 3358, 3366, 3367, 3368, 3369, 3370, 3391, 3395, 3400, 3402, 3451, 3456, 3458, 3460, 3461, 3544, 3552, 3560, 3561, 3562, 3565, 3567, 3578, 3586, 3587, 3588, 3597, 3604, 3605, 3607, 3613, 3617, 3623, 3629, 3636, 3638, 3645, 3669, 3675, 3698, 3701, 3719, 3737, 3745, 3778, 3779, 3802, 3817, 3818, 3819, 3821, 3828, 3829, 3831, 3838, 3843, 3847, 3853, 3857, 3864, 3866, 3872, 3885, 3892, 3930, 3934, 3935, 3937, 3942, 3944, 3966, 3974, 3975, 3976, 3977, 3979, 3995, 4000, 4008, 4012, 4013, 4014, 4016, 4025, 4030, 4034, 4036, 4047, 4055, 4085, 4088, 4093, 4096, 4099, 4109, 4112, 4113, 4114, 4126, 4143, 4170, 4174, 4179, 4215, 4222, 4231, 4247, 4259, 4289, 4314, 4323, 4364, 4371, 4373, 4375, 4376, 4377, 4390, 4397, 4399, 4401, 4402, 4403, 4413, 4420, 4422, 4424, 4425, 4427, 4429, 4455, 4460, 4476, 4477, 4485, 4486, 4492, 4502, 4506, 4510, 4541, 4548, 4550, 4552, 4593, 4599, 4604, 4605, 4610, 4611, 4616, 4632, 4640, 4645, 4647, 4665, 4691, 4695, 4697, 4699, 4712, 4717, 4718, 4720, 4722, 4730, 4751, 4801, 4838, 4852, 4908, 4913, 4915, 4916, 4918, 4946, 4950, 4952, 4954, 4955, 4956, 4962, 4965, 4977, 4984, 4986, 4988, 4991, 4992, 4996, 4998, 4999, 5003, 5005, 5007, 5011, 5035, 5058, 5178, 5187, 5188, 5194, 5195, 5201, 5263, 5269, 5278, 5281, 5283, 5286, 5289, 5292, 5407, 5429, 5438, 5457, 5545, 5558, 5565, 5566, 5570, 5572, 5575, 5578, 5594, 5609, 5616, 5621, 5633, 5638, 5642, 5648, 5650, 5661, 5667, 5669, 5678, 5690, 5786, 5928, 5933, 5935, 5936, 5938, 5965, 5973, 5975, 5977, 5980, 5981, 5985, 5987, 5988, 5992, 5995, 5999, 6035, 6038, 6064, 6065, 6069, 6071, 6073, 6074, 6075, 6084
- `crates/ui/src/work_outline/chips.rs`: 18, 21, 22, 23, 24, 25, 93, 101, 103, 105, 127
- `crates/ui/src/work_outline/details_artifacts.rs`: 324, 336, 338, 376, 384, 386, 387, 399, 403, 417, 420, 421, 422, 423, 430, 432, 495, 507, 514, 516, 534, 539, 541, 543, 552, 557, 558, 559, 560
- `crates/ui/src/work_outline/details_container_template.rs`: 517, 518, 519, 550, 568, 576, 578, 589, 594, 596, 617, 619, 624, 662, 663, 670, 739, 743, 760
- `crates/ui/src/work_outline/details_convert.rs`: 221, 222, 231, 237, 242, 243, 244, 246, 248, 276, 281, 292, 298, 299, 313
- `crates/ui/src/work_outline/details_execution.rs`: 554, 560, 561, 562, 564, 576, 577, 583, 589, 590, 595, 596, 640, 642, 662, 667, 668, 673, 674, 699, 705, 706, 719, 725, 726, 729, 733, 736, 741, 743, 749, 751, 780, 785, 787, 792, 795, 807, 808, 817, 818, 830, 835, 837, 843, 853, 930, 936, 937, 938, 940, 945, 947, 953, 957, 969, 972, 975, 990, 1006, 1019, 1027, 1028, 1029, 1031, 1033, 1045, 1047, 1048, 1049, 1050, 1064, 1065, 1071, 1078, 1089, 1096, 1101, 1121, 1138, 1188, 1203, 1209, 1223, 1247, 1249
- `crates/ui/src/work_outline/details_friction.rs`: 242, 248, 249, 280, 296, 315, 354, 356, 364, 366, 372, 375, 397, 402, 408, 412, 418, 427, 446, 448, 449, 451, 458, 464, 473, 489, 527, 555, 699, 712
- `crates/ui/src/work_outline/details_instructions.rs`: 410, 421, 451, 475, 482, 486, 491, 492, 493, 541, 548, 549, 553, 560, 562, 568, 638, 644, 645, 646, 647, 654, 661, 662, 664, 677, 681, 694, 695, 701, 716, 722, 723, 724, 736, 747, 749, 766, 789, 791, 803, 805, 812, 813, 826, 834, 838, 840, 845, 850, 852, 855, 861, 880, 882, 894, 896, 906, 908, 959, 964, 967, 977, 1087, 1088, 1098, 1099, 1110, 1128, 1133, 1151, 1159, 1163, 1170, 1172, 1188, 1200, 1201
- `crates/ui/src/work_outline/details_linear.rs`: 325, 329, 360, 361, 366, 368, 413, 419, 423, 473, 479, 492, 512, 513, 520, 522, 540, 547, 548, 549, 551, 553, 596, 602, 607, 609, 617, 618, 628, 629, 635, 640, 641, 654, 656
- `crates/ui/src/work_outline/details_panel.rs`: 805, 806, 811, 812, 862, 863, 872, 880, 884, 885, 886, 888, 894, 904, 905, 1322, 1324, 1326, 1341, 1355, 1367, 1382, 1387, 1395, 1403, 1411, 1445, 1446, 1462, 1478, 1503, 1523, 1549, 1552, 1596, 1604, 1605, 1608, 1613, 1657, 1664, 1665, 1671, 1678, 1681, 1683, 1688, 1693, 1720, 1725, 1726, 1735, 1742, 1744, 1745, 1747, 1750, 1771, 1794, 1806, 1812, 1834, 1847, 1864, 1915, 1917, 1922, 1942, 1944
- `crates/ui/src/work_outline/details_pr_sync.rs`: 125, 131, 132, 138, 143, 145, 154, 159, 201, 202, 226, 246, 250, 263, 265, 295, 303, 305, 311, 313, 319, 325, 335, 336, 429, 435, 440, 442, 464, 465, 469, 476, 486, 500, 502
- `crates/ui/src/work_outline/details_template_review.rs`: 302, 330, 331, 342, 344, 351, 394, 400, 403, 408, 411, 417, 419
- `crates/ui/src/work_outline/details_templates.rs`: 252, 253, 259, 272, 274, 280, 315, 321, 322, 328, 333, 334, 349, 355, 361, 409, 416, 419
- `crates/ui/src/work_outline/details_user_actions.rs`: 469, 474, 478, 536, 556, 559, 604, 630, 632, 641, 646, 667, 670, 717, 732, 733, 742, 745, 789, 791, 804, 806, 831, 833, 834, 854, 859, 865, 866, 920, 972, 984, 985, 986, 995, 1020, 1076, 1077, 1083, 1111, 1113, 1126, 1132, 1195, 1201, 1202, 1214, 1215
- `crates/ui/src/work_outline/hints.rs`: 199, 205, 206, 207, 208, 209, 479, 480, 481
- `crates/ui/src/work_outline/linear_outline.rs`: 33, 246, 251, 252, 253, 255, 257, 356, 364, 366, 368, 372, 377, 380, 544, 549, 566, 586, 630, 639, 641, 643, 644, 645, 662, 671, 673, 675, 681, 691, 699, 700, 727, 733, 734, 735, 736, 738
- `crates/ui/src/work_outline/outline_view/move_toast.rs`: 265, 270, 272, 276, 286, 287, 288, 289, 290, 292
- `crates/ui/src/work_outline/outline_view/templates.rs`: 476, 481, 483, 487, 497, 498, 499, 500, 501, 503
- `crates/ui/src/work_outline/outline_view/tree_guides_view.rs`: 196
- `crates/ui/src/work_outline/outline_view.rs`: 442, 446, 447, 448, 449, 450, 2593, 2602, 2649, 2663, 2670, 2671, 2672, 2673, 2781, 2797, 2808, 2810, 2834, 2843, 2850, 2859, 2861, 2866, 2874, 2876, 2878, 2906, 2913, 2932, 2942, 2944, 2946, 2963, 2970, 2977, 2979, 2984, 2988, 3131, 3133, 3136, 3139, 3144, 3314, 3318, 3321, 3323, 3329, 3351, 3359, 3361, 3363, 3364, 3366, 3376, 3388, 3389, 3390, 3391, 3396, 3401, 3404, 3412, 3415, 3417, 3425, 3455, 3494, 3501, 3509, 3515, 3516, 3527, 3540, 3541, 3583, 3592, 3602, 3612, 3627, 3639, 3652, 3688, 3694, 3707, 3711, 3712, 3714, 3730, 3743, 3762, 3771, 3781, 3787, 3794, 3799, 3804, 3816, 3820, 3821, 3823, 3866, 3876, 3888, 3939, 3947, 3952, 4085, 4106, 4111
- `crates/ui/src/work_outline/pr_sync_outline.rs`: 319, 326, 339, 347, 348
- `crates/ui/src/work_outline/required_actions_view.rs`: 1074, 1246, 1251, 1254, 1269, 1271, 1286, 1287, 1290, 1325, 1329, 1363, 1527, 1532, 1537, 1538, 1539, 1561, 1572, 1578, 1579, 1580, 1581, 1585, 1674, 1678, 1680, 1713, 1802, 1807, 1822, 1826, 1833, 1834, 1835, 1839, 1841, 1850, 1856, 1862, 1868, 1907, 1912, 1913, 1914, 1916, 1918, 1952, 1994, 2003, 2005, 2007, 2009, 2020, 2040, 2044, 2047, 2060, 2061, 2062, 2063, 2071, 2117, 2124, 2126, 2127, 2138, 2146, 2176, 2182, 2252, 2259, 2271, 2277, 2285, 2287, 2294, 2303, 2304, 2305, 2318, 2327, 2330, 2337, 2340, 2349
- `crates/ui/src/work_outline/revision_diff.rs`: 109, 114, 128, 139, 145, 150, 153, 157, 162, 169, 186, 191, 192, 193, 203, 206, 220, 226, 227, 238, 240, 252, 260, 261, 320, 321

The remaining detail sub-surfaces represented by that index include execution
validation, exceptions, binding and editor controls (`work_outline/details_execution.rs:582,685,767,896,1076,1171`); friction (`details_friction.rs:233`); Linear links and setup (`details_linear.rs:343,531,575`); PR sync (`details_pr_sync.rs:103,275,347`); artifacts and their menu (`details_artifacts.rs:282,524`); conversion (`details_convert.rs:204`); template review (`details_template_review.rs:283,377`); and instruction revision comparisons (`revision_diff.rs:96,179,248`). The renderer and its children should be checked together: section wrappers are not evidence that their rows or controls already use a component.

### Convergence triage

The index above records all presentation expressions at `4dc1911d`; subsequent
commits moved the style-sensitive outline and item-detail controls into
`components/outline_parts.rs`. The remaining indexed expressions stay literal,
in the baseline: structural flex containers and spacers (geometry controlled by
the parent component), data-dependent text and semantic colors (content rather
than an appearance selection), tooltip plumbing and event hit targets, and
Overview chat/canvas renderers outside the requested item-panel surfaces. The
index includes tests embedded in production files. These are not claims that
all inline expressions were eliminated; future migrations can reduce the
ratchet further. A direct scan of the owned production files found no branches
on `UiStyleId`, `UiLayout`, or the appearance control, surface, status,
conversation, and behavior variants.

### Shell follow-ups

- Restore project, branch and remote metadata parity in Area sidebar rows.
- Triage the remaining mechanical candidate ledger below; Item/Team panels were not visually checked in the chat preview.

### Shell mechanical candidate ledger (untriaged)

Untriaged; no per-style rendering difference observed for the remaining visible candidates in preview `target-clean/ui-preview/20261106-181557-290/chat.png`. The sheet does not cover every candidate or open the Item/Team panels.

Locations refer to shell branch `d311aee00`. This index includes tests and already migrated code, and can miss inline elements without these patterns. It is a navigation aid, not a completed manual audit. Interaction includes click, mouse-down and key-down sites; Label covers explicit text sizes; Radius covers explicit rounded shapes; ID covers element IDs.

**Stays literal, in baseline:** event handlers and element IDs preserve control
identity and behavior; drag, scroll, viewport and window-hit-test geometry is
structural rather than a style choice. Test fixtures retain explicit setup and
assertion values. The remaining label/radius candidates have not all been
individually triaged, so this ledger must not be read as a completed audit.

| File | Interaction lines | Label lines | Radius lines | ID lines |
| --- | --- | --- | --- | --- |
| `shell.rs` | 6567, 6589, 7010, 7204, 7233, 7341, 7342, 7527, 7528, 7615, 7618, 8420, 8465, 8562, 8566, 8602, 8626, 8638, 8653, 8731, 8738, 8780, 8789, 8796, 8804, 8902, 8936, 8943, 8984, 8994, 9018, 9026, 9267, 9276, 9286, 9295, 9304, 9317, 9331, 9345, 9364, 9385, 9399, 9412, 9430, 9463, 9480, 9498, 9517, 9545, 9568, 9576, 9631, 9639, 9730, 9966, 10217, 10320, 10660, 10665, 10674, 10680, 10751, 10939, 10942, 10948, 10976, 10980, 11079, 11086, 11113, 11127, 11142, 11156, 11291, 11349, 11426, 11462, 11548, 11575, 11956, 15353 | 1106, 1128, 6519, 6539, 7163, 7190, 7229, 7407, 7445, 7480, 7663, 7708, 7789, 8264, 8283, 8417, 8592, 8851, 8920, 8968, 9948, 9956, 10137, 10234, 10240, 10401, 10447, 10640, 10719, 10728, 10739, 11275, 11287, 11318, 11327, 11344, 11439, 11457, 11499, 11508, 11530, 11542, 11561, 11571, 11934 | 1102, 1124, 6668, 6981, 7187, 7226, 7405, 7424, 7583, 8414, 8864, 8869, 10210, 10225, 10627, 10702, 11284, 11301, 11342, 11420, 11453, 11488, 11526, 11540, 11930 | 909, 6560, 6787, 7179, 7221, 7498, 7567, 7835, 8195, 8208, 8304, 8379, 8409, 8601, 8613, 8625, 8637, 8652, 8730, 8737, 8779, 8788, 8795, 8803, 8901, 8935, 8942, 8983, 8990, 9017, 9025, 9265, 9275, 9285, 9294, 9303, 9316, 9330, 9344, 9363, 9384, 9398, 9411, 9428, 9462, 9479, 9497, 9516, 9567, 9575, 9630, 9638, 9698, 9964, 9985, 10108, 10129, 10208, 10305, 10623, 10747, 10817, 11112, 11126, 11141, 11155, 11281, 11336, 11417, 11450, 11535, 11570, 11916 |
| `rail.rs` | 536 | — | — | — |
| `shell/actions_ui.rs` | 567, 589, 599, 605, 618, 634, 685, 701, 733, 755, 781, 824, 834, 853, 896, 919, 964, 979, 991, 1030, 1048, 1049, 1110 | 677, 772, 948, 1027, 1064 | 556, 731, 883, 928 | 26, 684, 699, 723, 754, 780, 823, 833, 878, 913, 962, 978, 989, 1021, 1038, 1092, 1102, 1108, 1120 |
| `shell/admin_ui.rs` | 161, 199, 214, 220, 286, 319 | 148, 187, 265, 315 | 146, 185, 262, 280, 313 | 139, 177, 212, 218, 249, 276, 309 |
| `shell/chat_item_ui.rs` | 254, 275, 322, 341, 419, 505, 529, 560, 692, 727, 851, 993, 1004, 1050, 1105, 1144, 1238, 1327, 1338, 1386, 1427, 1471, 1489, 1585, 1623 | 536, 798, 844, 951, 1305, 1558, 1615, 1737, 1747, 1766, 1803 | 776, 947, 1086, 1301, 1452, 1580, 1612, 1757 | 1576, 1610 |
| `shell/closeout_ui.rs` | 368, 480, 537, 585, 647, 666, 693 | 381, 534, 582 | 356, 458, 476, 512, 529, 573 | 367, 470, 525, 570, 641, 665, 674, 692 |
| `shell/command_palette.rs` | 322, 438 | 415, 507, 565 | 317, 432 | 272, 316, 396, 427 |
| `shell/files_panel.rs` | — | — | — | 312 |
| `shell/hand_off_ui.rs` | 145, 150, 151 | — | — | 144, 145, 150, 151 |
| `shell/instructions_ui.rs` | 419, 444, 479, 492, 499, 1165, 1183, 1198, 1206, 1231, 1254, 1355, 1370 | 392, 1053 | 373, 1335 | 369, 426, 1114, 1163, 1180, 1196, 1204, 1214, 1228, 1250, 1353, 1368 |
| `shell/outside_file_ui.rs` | 211, 217 | 197 | 169, 193 | 165, 179 |
| `shell/project_icon.rs` | — | 124, 245, 255 | 107, 235 | 273 |
| `shell/required_actions_ui.rs` | 124 | 105 | 119 | 115 |
| `shell/review_ui.rs` | 53 | — | — | 50 |
| `shell/running_ui.rs` | 565, 616, 767, 771, 777, 789, 793, 806, 813, 817, 865, 893, 906, 947, 958, 1022, 1070 | 626, 729, 750, 876, 1003, 1115, 1130, 1150, 1157, 1182 | 557, 709, 920 | 549, 592, 858, 1019, 1067 |
| `shell/sidebar_sections.rs` | 288, 322, 437, 476, 499, 523, 554, 566 | 308, 352 | 320 | 277, 315, 367, 475, 517, 553, 564 |
| `shell/spaces.rs` | 3445, 3531, 3638, 3679, 3695, 3742, 3746, 3840, 3844, 3851, 3906, 4028, 4032, 4074, 4112, 4754, 4850, 4929, 5043, 5115, 5930, 5955, 5976, 6030, 6117, 6135, 6157, 6173, 6199, 6276, 6297, 6478, 6500, 6512, 6539, 6562, 6570, 6614, 6623 | 1959, 1983, 3782, 4047, 5111, 6052, 6186, 6232, 6279 | 1954, 1978, 2669, 3828, 3898, 5110, 5912, 5936, 6062, 6195, 6291 | 3441, 3530, 3582, 3607, 3635, 3654, 3678, 4027, 4111, 4636, 4753, 4825, 4876, 4905, 4949, 5042, 5100, 5911, 6008, 6029, 6059, 6189, 6265, 6286, 6475, 6499, 6510, 6561, 6569, 6612, 6621 |
| `shell/start_chat_ui.rs` | 1114, 1139, 1204, 1299, 1331, 1454, 1533, 1550, 1616, 1625, 1655, 1666, 1670, 1739, 1789, 1798 | 1038, 1111, 1281, 1287, 1438, 1483, 1494, 1705 | 1413, 1579, 1701 | 1105, 1135, 1203, 1297, 1327, 1431, 1531, 1561, 1614, 1622, 1665, 1668, 1746, 1787, 1795 |
| `shell/tabs.rs` | — | — | — | 349 |
| `shell/team_ui.rs` | 95, 224, 241, 298, 305, 360, 420, 440, 624, 711, 722, 756, 780, 806 | 139, 148, 156, 217, 235, 354, 373, 387, 460, 486, 595, 619, 664, 683, 696, 740, 801 | 616, 659 | 214, 232, 346, 609 |
| `shell/wake_ui.rs` | 194, 228 | 293 | — | — |
| `shell/work_outline_menu_ui.rs` | 643, 721 | — | — | 694 |
| `shell/work_outline_ui.rs` | 609, 1232, 1240, 1270, 1295, 1339, 1390, 1419, 1442, 1456 | 536, 548, 636, 646, 676, 1217 | 632 | 564, 607, 1230, 1238, 1268, 1293, 1335, 1375, 1389, 1417, 1426, 1440 |
| `shell/workspace_search.rs` | 758, 809, 919 | 912 | 755, 806 | 748, 801, 898 |
### Conversation and composer: initial audit (incomplete)

Line numbers refer to integration commit `d68928981`. This is a starting map,
not a complete inventory of every inline element.

- Transcript: user body and expander `transcript.rs:5478`, `:5617`; attachments
  `:5748`; row and style-specific message shells `:6203`, `:6211`, `:6334`,
  `:6344`, `:6345`, `:6573`; tool group `:6845`; error and input chips `:7776`,
  `:7795`; tool headers and chips `:8147`, `:8412`, `:8603`, `:8668`.
- Composer: skill, comment and attachment chips `composer.rs:4937`, `:5300`,
  `:5328`, `:5420`; input `:6062`; attach button and footer `:6432`, `:6479`;
  wizard surface `:7985`, `:7989`; send variants and layout `:8097`, `:8103`;
  main render style, radius, key, hints, readout and body branches `:8200`,
  `:8627`, `:8693`, `:8742`, `:8774`, `:8803`, `:8846`; dock `composer_dock.rs:323`.
- Markdown: block layout and body tokens `markdown/render.rs:478`, `:2332`;
  code panel and actions `:2448`, `:2507`, `:2542`, `:2590`, `:2637`,
  `:2684`; Area-specific code treatments `:2458`, `:2585`, `:2598`, `:2654`,
  `:2975`; question cards `:675`.
- Further audit is needed for inline message actions, all tool details, composer
  tabs and selectors, and the rest of `composer/` and `markdown/`.
- Additional candidates: `composer/skill_completion.rs:638` completion rows;
  `composer/instruction_reset.rs:643` reset prompt; `markdown/link_destination.rs:26`
  destination card; `markdown/render.rs:565` answer button, `:596` answer
  affordance, `:674` question card, `:945` block contents, `:1220` table,
  `:2534` code copy action and `:2604` code source; `transcript.rs:7821`
  tool-patch header, `:7877` detail body, `:8015` more-lines row, `:8087`
  tool header, `:8443` activity rail and `:8543` tool chip. These references
  are from `d68928981` and need individual review as migration continues.

#### Remaining conversation call sites at `78e1a10a8`

These are grouped by semantic surface; line numbers identify the owning render
function or its first inline element, rather than every child `div`.

- User content: source header and compact relay `transcript.rs:6202`; attachment
  strip `:5747`; expanded body and disclosure `:5477`, `:5616`; inline mention
  text and underlays `:7430`; generated image and visualization cards `:5677`,
  `:7520`; metadata lane `:6550`.
- Tools and thought: group disclosure and tree `transcript.rs:6784`; expanded
  invocation, output and thought body `:7810`; patch file header `:7744`;
  truncation row `:7938`; chip content `:8010`, `:8246`; connector and reveal
  geometry `:8320`; tool and subagent rows `:8437`, `:8483`.
- Composer: skill chip row `composer.rs:4937`; comments badge `:5271`;
  attachment and appshot strips `:5299`, `:5391`; mention and slash menus
  `:5895`, `:6281`, `:6403`, `:6450`; editable input `:3854`, `:6033`;
  question wizard controls `:7865`; composer and footer assembly `:8109`;
  completion rows `composer/skill_completion.rs:638` and reset prompt
  `composer/instruction_reset.rs:643`. `composer_dock.rs:323` owns the custom
  docking geometry rather than a separate visible control.
- Markdown: answer controls and question cards `markdown/render.rs:565`, `:596`,
  `:674`; blocks, inline formatting and tables `:945`, `:1220`, `:1381`;
  code source and selection `:2536`, `:2558`; link preview card
  `markdown/link_destination.rs:26`.
- The production slice has no direct style or layout variant comparisons at
  this revision. `markdown/render.rs:2906` is an Area-specific test setup.
  Theme-only and semantic state branches remain; a component wrapper does not
  by itself migrate all of the child elements listed above.
- Composer-facing picker audit (`0401e0448`): approval chip `pickers.rs:1389`,
  model/harness trigger `:3731`, footer triggers `:3812`, locked footer labels
  `:4057`, `:4150`, `:4161`, footer assembly `:4085`, and model/approval chip
  assembly `:6019`. Their visual frames now live in `conversation_parts.rs`;
  picker popover and menu bodies remain outside this slice. No direct style or
  layout comparisons were present in these picker sites before migration.

## Browser and artifacts audit

This historical index records presentation candidates and style references at the integration base. Shared browser parts now own the direct style branches, toolbar, picker and navigation controls, cards, badges, and preview surfaces. Separators and native web-page backing remain theme-colored structural boundaries. The current fixed text-size literals are listed below the historical index.

- `crates/ui/src/browser/fragment.rs:118`: `("--input", css(theme.border_strong)),`
- `crates/ui/src/browser/fragment_view.rs:253`: `crate::files::toolbar_button(id, text, theme)`
- `crates/ui/src/browser/fragment_view.rs:418`: `.border_b_1()`
- `crates/ui/src/browser/fragment_view.rs:419`: `.border_color(theme.border)`
- `crates/ui/src/browser/fragment_view.rs:437`: `.rounded(px(6.0))`
- `crates/ui/src/browser/fragment_view.rs:438`: `.border_1()`
- `crates/ui/src/browser/fragment_view.rs:439`: `.border_color(theme.border)`
- `crates/ui/src/browser/fragment_view.rs:440`: `.bg(theme.surface_raised)`
- `crates/ui/src/browser/fragment_view.rs:444`: `.hover(|style| style.bg(crate::theme::wash(0.10)))`
- `crates/ui/src/browser/view.rs:8`: `fn button(`
- `crates/ui/src/browser/view.rs:18`: `crate::files::toolbar_button(id, label, theme)`
- `crates/ui/src/browser/view.rs:130`: `let border_strong = theme.border_strong;`
- `crates/ui/src/browser/view.rs:145`: `.rounded(px(10.0))`
- `crates/ui/src/browser/view.rs:146`: `.border_1()`
- `crates/ui/src/browser/view.rs:147`: `.border_color(theme.border)`
- `crates/ui/src/browser/view.rs:148`: `.bg(crate::theme::ink(0.02))`
- `crates/ui/src/browser/view.rs:156`: `.bg(crate::theme::ink(0.05))`
- `crates/ui/src/browser/view.rs:157`: `.border_color(border_strong)`
- `crates/ui/src/browser/view.rs:209`: `.rounded(px(6.0))`
- `crates/ui/src/browser/view.rs:219`: `.hover(|style| style.bg(crate::theme::ink(0.05)))`
- `crates/ui/src/browser/view.rs:241`: `.rounded(px(8.0))`
- `crates/ui/src/browser/view.rs:242`: `.border_1()`
- `crates/ui/src/browser/view.rs:243`: `.border_color(theme.border)`
- `crates/ui/src/browser/view.rs:320`: `.rounded(px(12.0))`
- `crates/ui/src/browser/view.rs:321`: `.border_1()`
- `crates/ui/src/browser/view.rs:322`: `.border_color(theme.border)`
- `crates/ui/src/browser/view.rs:323`: `.bg(theme.surface_raised.opacity(0.5))`
- `crates/ui/src/browser/view.rs:355`: `.rounded(px(6.0))`
- `crates/ui/src/browser/view.rs:356`: `.border_1()`
- `crates/ui/src/browser/view.rs:357`: `.border_color(theme.border)`
- `crates/ui/src/browser/view.rs:358`: `.bg(theme.surface_raised)`
- `crates/ui/src/browser/view.rs:366`: `.hover(|style| style.bg(crate::theme::wash(0.10)))`
- `crates/ui/src/browser/view.rs:408`: `let back = button(`
- `crates/ui/src/browser/view.rs:419`: `let forward = button(`
- `crates/ui/src/browser/view.rs:430`: `let reload = button(`
- `crates/ui/src/browser/view.rs:444`: `el.border_1().border_color(theme.danger)`
- `crates/ui/src/browser/view.rs:477`: `.rounded(px(4.0))`
- `crates/ui/src/browser/view.rs:484`: `.hover(|s| s.bg(crate::theme::wash(0.10)))`
- `crates/ui/src/browser/view.rs:494`: `let open = button(`
- `crates/ui/src/browser/view.rs:525`: `.when(has_page, |body| body.bg(theme.bg));`
- `crates/ui/src/browser/view.rs:672`: `.when(remote_loopback, |el| el.child(div().px(px(12.0)).py(px(8.0)).border_b_1().border_color(theme.border)`
- `crates/ui/src/browser/view.rs:676`: `.when(external, |el| el.child(div().h(px(26.0)).px(px(10.0)).flex().items_center().gap(px(5.0)).border_t_1().border_color(theme.border)`
- `crates/ui/src/artifacts/cards.rs:113`: `.rounded(px(10.0))`
- `crates/ui/src/artifacts/cards.rs:114`: `.border_1()`
- `crates/ui/src/artifacts/cards.rs:115`: `.border_color(theme.border)`
- `crates/ui/src/artifacts/cards.rs:116`: `.bg(theme.surface_raised)`
- `crates/ui/src/artifacts/cards.rs:122`: `.hover(|card| card.bg(theme.element_hover))`
- `crates/ui/src/artifacts/cards.rs:127`: `.rounded(px(7.0))`
- `crates/ui/src/artifacts/cards.rs:128`: `.bg(theme.accent.opacity(0.10))`
- `crates/ui/src/artifacts/cards.rs:166`: `.rounded(px(6.0))`
- `crates/ui/src/artifacts/cards.rs:167`: `.border_1()`
- `crates/ui/src/artifacts/cards.rs:168`: `.border_color(theme.border)`
- `crates/ui/src/artifacts/cards.rs:171`: `.hover(|el| el.bg(theme.element_hover))`
- `crates/ui/src/artifacts/plan_linear.rs:167`: `.rounded(px(6.0))`
- `crates/ui/src/artifacts/plan_linear.rs:168`: `.border_1()`
- `crates/ui/src/artifacts/plan_linear.rs:169`: `.border_color(theme.border)`
- `crates/ui/src/artifacts/plan_linear.rs:170`: `.bg(theme.surface_card)`
- `crates/ui/src/artifacts/plan_linear.rs:245`: `.rounded(px(3.0))`
- `crates/ui/src/artifacts/plan_linear.rs:246`: `.border_1()`
- `crates/ui/src/artifacts/plan_linear.rs:247`: `.border_color(theme.border)`
- `crates/ui/src/artifacts/plan_linear.rs:266`: `.rounded(px(2.0))`
- `crates/ui/src/artifacts/plan_linear.rs:267`: `.bg(crate::theme::wash(0.12))`
- `crates/ui/src/artifacts/plan_linear.rs:271`: `.rounded(px(2.0))`
- `crates/ui/src/artifacts/plan_linear.rs:272`: `.bg(theme.accent)`
- `crates/ui/src/artifacts/questions_ui.rs:167`: `.rounded_full()`
- `crates/ui/src/artifacts/questions_ui.rs:168`: `.bg(theme.accent)`
- `crates/ui/src/artifacts/tab/comments_view.rs:426`: `.rounded(px(surface_chrome::CONTROL_RADIUS))`
- `crates/ui/src/artifacts/tab/comments_view.rs:434`: `.when(on, |el| el.bg(crate::theme::wash(0.10)))`
- `crates/ui/src/artifacts/tab/comments_view.rs:436`: `el.hover(|el| el.bg(crate::theme::wash(0.14)).text_color(theme.text))`
- `crates/ui/src/artifacts/tab/comments_view.rs:558`: `.border_l_1()`
- `crates/ui/src/artifacts/tab/comments_view.rs:559`: `.border_color(theme.border)`
- `crates/ui/src/artifacts/tab/comments_view.rs:565`: `.border_b_1()`
- `crates/ui/src/artifacts/tab/comments_view.rs:566`: `.border_color(theme.border)`
- `crates/ui/src/artifacts/tab/comments_view.rs:597`: `.rounded(px(6.0))`
- `crates/ui/src/artifacts/tab/comments_view.rs:598`: `.border_1()`
- `crates/ui/src/artifacts/tab/comments_view.rs:599`: `.border_color(theme.accent.opacity(0.5))`
- `crates/ui/src/artifacts/tab/comments_view.rs:669`: `.rounded(px(6.0))`
- `crates/ui/src/artifacts/tab/comments_view.rs:670`: `.border_1()`
- `crates/ui/src/artifacts/tab/comments_view.rs:671`: `.border_color(if focused { theme.accent } else { theme.border })`
- `crates/ui/src/artifacts/tab/comments_view.rs:694`: `.rounded(px(8.0))`
- `crates/ui/src/artifacts/tab/comments_view.rs:695`: `.border_1()`
- `crates/ui/src/artifacts/tab/comments_view.rs:696`: `.border_color(color.opacity(0.5))`
- `crates/ui/src/artifacts/tab/comments_view.rs:697`: `.bg(color.opacity(0.10))`
- `crates/ui/src/artifacts/tab/comments_view.rs:757`: `.rounded(px(5.0))`
- `crates/ui/src/artifacts/tab/comments_view.rs:758`: `.border_1()`
- `crates/ui/src/artifacts/tab/comments_view.rs:759`: `.border_color(if primary { theme.accent } else { theme.border })`
- `crates/ui/src/artifacts/tab/comments_view.rs:761`: `el.bg(theme.accent).text_color(theme.on_accent)`
- `crates/ui/src/artifacts/tab/compare_view.rs:978`: `.border_b_1()`
- `crates/ui/src/artifacts/tab/compare_view.rs:979`: `.border_color(theme.border)`
- `crates/ui/src/artifacts/tab/compare_view.rs:1075`: `.border_b_1()`
- `crates/ui/src/artifacts/tab/compare_view.rs:1076`: `.border_color(theme.border)`
- `crates/ui/src/artifacts/tab/compare_view.rs:1271`: `.hover(|el| el.bg(theme.element_hover))`
- `crates/ui/src/artifacts/tab/compare_view.rs:1272`: `.when(selected, |el| el.bg(crate::theme::wash(0.08)))`
- `crates/ui/src/artifacts/tab/compare_view.rs:1297`: `.border_b_1()`
- `crates/ui/src/artifacts/tab/compare_view.rs:1298`: `.border_color(theme.border)`
- `crates/ui/src/artifacts/tab/compare_view.rs:1299`: `.bg(theme.surface_raised)`
- `crates/ui/src/artifacts/tab/compare_view.rs:1437`: `.border_b_1()`
- `crates/ui/src/artifacts/tab/compare_view.rs:1438`: `.border_color(theme.border)`
- `crates/ui/src/artifacts/tab/compare_view.rs:1608`: `.rounded(px(surface_chrome::CONTROL_RADIUS))`
- `crates/ui/src/artifacts/tab/compare_view.rs:1614`: `.when(active, |el| el.bg(crate::theme::wash(0.12)))`
- `crates/ui/src/artifacts/tab/compare_view.rs:1615`: `.hover(|el| el.bg(crate::theme::wash(0.14)).text_color(theme.text))`
- `crates/ui/src/artifacts/tab/find_view.rs:762`: `.rounded(px(surface_chrome::CONTROL_RADIUS))`
- `crates/ui/src/artifacts/tab/find_view.rs:768`: `.hover(|el| el.bg(crate::theme::wash(0.14)))`
- `crates/ui/src/artifacts/tab/find_view.rs:786`: `.border_b_1()`
- `crates/ui/src/artifacts/tab/find_view.rs:787`: `.border_color(theme.border)`
- `crates/ui/src/artifacts/tab/find_view.rs:815`: `button("artifact-find-prev", icons::ALT_ARROW_UP, has_matches)`
- `crates/ui/src/artifacts/tab/find_view.rs:819`: `button("artifact-find-next", icons::ALT_ARROW_DOWN, has_matches)`
- `crates/ui/src/artifacts/tab/find_view.rs:823`: `button("artifact-find-close", icons::CLOSE, true)`
- `crates/ui/src/artifacts/tab/find_view.rs:864`: `.border_r_1()`
- `crates/ui/src/artifacts/tab/find_view.rs:865`: `.border_color(theme.border)`
- `crates/ui/src/artifacts/tab/find_view.rs:871`: `.rounded(px(surface_chrome::CONTROL_RADIUS))`
- `crates/ui/src/artifacts/tab/find_view.rs:876`: `.hover(|el| el.bg(crate::theme::wash(0.14)))`
- `crates/ui/src/artifacts/tab/find_view.rs:902`: `.rounded(px(4.0))`
- `crates/ui/src/artifacts/tab/find_view.rs:915`: `el.bg(crate::theme::wash(if rail_focused { 0.20 } else { 0.10 }))`
- `crates/ui/src/artifacts/tab/find_view.rs:917`: `.hover(|el| el.bg(crate::theme::wash(0.14)))`
- `crates/ui/src/artifacts/tab/find_view.rs:957`: `.border_r_1()`
- `crates/ui/src/artifacts/tab/find_view.rs:958`: `.border_color(theme.border)`
- `crates/ui/src/artifacts/tab/find_view.rs:975`: `.rounded(px(surface_chrome::CONTROL_RADIUS))`
- `crates/ui/src/artifacts/tab/find_view.rs:980`: `.hover(|el| el.bg(crate::theme::wash(0.14)))`
- `crates/ui/src/artifacts/tab/options_view.rs:519`: `.rounded(px(5.0))`
- `crates/ui/src/artifacts/tab/options_view.rs:525`: `.when(strong, |el| el.bg(theme.accent).text_color(theme.surface))`
- `crates/ui/src/artifacts/tab/options_view.rs:527`: `el.border_1()`
- `crates/ui/src/artifacts/tab/options_view.rs:528`: `.border_color(theme.border)`
- `crates/ui/src/artifacts/tab/options_view.rs:530`: `.hover(|el| el.text_color(theme.text).bg(crate::theme::wash(0.10)))`
- `crates/ui/src/artifacts/tab/options_view.rs:588`: `.rounded(px(4.0))`
- `crates/ui/src/artifacts/tab/options_view.rs:591`: `.border_1()`
- `crates/ui/src/artifacts/tab/options_view.rs:592`: `.border_color(if on { theme.accent } else { theme.border })`
- `crates/ui/src/artifacts/tab/options_view.rs:635`: `.rounded(px(4.0))`
- `crates/ui/src/artifacts/tab/options_view.rs:638`: `.border_1()`
- `crates/ui/src/artifacts/tab/options_view.rs:639`: `.border_color(if on { theme.accent } else { theme.border })`
- `crates/ui/src/artifacts/tab/options_view.rs:677`: `.border_b_1()`
- `crates/ui/src/artifacts/tab/options_view.rs:678`: `.border_color(theme.border)`
- `crates/ui/src/artifacts/tab/options_view.rs:699`: `.rounded(px(4.0))`
- `crates/ui/src/artifacts/tab/options_view.rs:700`: `.border_1()`
- `crates/ui/src/artifacts/tab/options_view.rs:701`: `.border_color(theme.accent)`
- `crates/ui/src/artifacts/tab/options_view.rs:722`: `.rounded(px(5.0))`
- `crates/ui/src/artifacts/tab/options_view.rs:723`: `.border_1()`
- `crates/ui/src/artifacts/tab/options_view.rs:724`: `.border_color(theme.border)`
- `crates/ui/src/artifacts/tab/plan_view.rs:97`: `.border_b_1()`
- `crates/ui/src/artifacts/tab/plan_view.rs:98`: `.border_color(theme.border)`
- `crates/ui/src/artifacts/tab/questions_view.rs:464`: `.border_b_1()`
- `crates/ui/src/artifacts/tab/questions_view.rs:465`: `.border_color(theme.border);`
- `crates/ui/src/artifacts/tab/questions_view.rs:528`: `.rounded(px(9.0))`
- `crates/ui/src/artifacts/tab/questions_view.rs:529`: `.border_1()`
- `crates/ui/src/artifacts/tab/questions_view.rs:530`: `.border_color(color.opacity(0.5))`
- `crates/ui/src/artifacts/tab/questions_view.rs:531`: `.bg(color.opacity(0.10))`
- `crates/ui/src/artifacts/tab/zoom_view.rs:392`: `.rounded(px(surface_chrome::CONTROL_RADIUS))`
- `crates/ui/src/artifacts/tab/zoom_view.rs:400`: `.hover(|el| el.bg(crate::theme::wash(0.14)).text_color(theme.text))`
- `crates/ui/src/artifacts/tab/zoom_view.rs:414`: `button(`
- `crates/ui/src/artifacts/tab/zoom_view.rs:424`: `button("artifact-zoom-reset", zoom::label(level).into(), !default)`
- `crates/ui/src/artifacts/tab/zoom_view.rs:432`: `button("artifact-zoom-in", "+".into(), level < zoom::MAX_ZOOM).on_click(`
- `crates/ui/src/artifacts/tab.rs:951`: `.border_b_1()`
- `crates/ui/src/artifacts/tab.rs:952`: `.border_color(theme.border)`
- `crates/ui/src/artifacts/tab.rs:1054`: `crate::files::toolbar_button(id, label, theme)`
- `crates/ui/src/artifacts/tab.rs:1062`: `.when(theme.ui_style.id == crate::ui_style::UiStyleId::Area, |el| el.rounded_full().border_1().border_color(theme.border_strong))`
- `crates/ui/src/artifacts/tab.rs:1084`: `.rounded(px(surface_chrome::CONTROL_RADIUS))`
- `crates/ui/src/artifacts/tab.rs:1091`: `.when(open, |el| el.bg(crate::theme::wash(0.10)))`
- `crates/ui/src/artifacts/tab.rs:1092`: `.hover(|el| el.bg(crate::theme::wash(0.14)).text_color(theme.text))`
- `crates/ui/src/artifacts/tab.rs:1098`: `.when(theme.ui_style.id == crate::ui_style::UiStyleId::Area, |toolbar| {`
- `crates/ui/src/artifacts/tab.rs:1099`: `toolbar.child(div().flex().items_center().rounded(px(12.0)).border_1().border_color(theme.border_strong).overflow_hidden()`
- `crates/ui/src/artifacts/tab.rs:1102`: `.when(revision.is_some() && id == "artifact-previous-revision", |el| el.bg(theme.accent))`
- `crates/ui/src/artifacts/tab.rs:1147`: `button(`
- `crates/ui/src/artifacts/tab.rs:1158`: `button("artifact-reveal", "Reveal in Finder", icons::FOLDER, local)`
- `crates/ui/src/artifacts/tab.rs:1164`: `button("artifact-source", "View source", icons::FILE_CODE, text).when(text, |el| {`
- `crates/ui/src/artifacts/tab.rs:1169`: `button(`
- `crates/ui/src/artifacts/tab.rs:1175`: `.when(pinned, |el| el.bg(crate::theme::wash(0.10)))`
- `crates/ui/src/artifacts/tab.rs:1183`: `button(`
- `crates/ui/src/artifacts/tab.rs:1199`: `if theme.ui_style.id != crate::ui_style::UiStyleId::Area { return None; }`
- `crates/ui/src/artifacts/tab.rs:1210`: `.when(local, |el| el.child(div().id("area-artifact-reveal").debug_selector(|| "area-artifact-reveal".into()).px(px(10.0)).py(px(5.0)).rounded_full().border_1().border_color(theme.border_strong).cursor_pointer().child("Reveal")`
- `crates/ui/src/artifacts/tab.rs:1212`: `.when(standalone, |el| el.child(div().id("area-artifact-open").debug_selector(|| "area-artifact-open".into()).px(px(10.0)).py(px(5.0)).rounded_full().bg(theme.accent).text_color(theme.identity_text(theme.accent)).cursor_pointer().child("Open")`
- `crates/ui/src/artifacts/tab.rs:1238`: `.hover(|el| el.bg(theme.element_hover))`
- `crates/ui/src/artifacts/tab.rs:1268`: `.hover(|el| el.bg(theme.element_hover))`
- `crates/ui/src/artifacts/tab.rs:1270`: `el.bg(crate::theme::wash(0.08))`
- `crates/ui/src/artifacts/tab.rs:1303`: `.border_b_1()`
- `crates/ui/src/artifacts/tab.rs:1304`: `.border_color(theme.border)`
- `crates/ui/src/artifacts/tab.rs:1305`: `.bg(theme.surface_raised)`
- `crates/ui/src/artifacts/tab.rs:1422`: `.rounded(px(6.0))`
- `crates/ui/src/artifacts/tab.rs:1423`: `.border_1()`
- `crates/ui/src/artifacts/tab.rs:1424`: `.border_color(theme.border)`
- `crates/ui/src/artifacts/tab.rs:1429`: `.hover(|el| el.bg(theme.element_hover))`
- `crates/ui/src/artifacts/tab.rs:1533`: `.rounded(px(6.0))`
- `crates/ui/src/artifacts/tab.rs:1534`: `.border_1()`
- `crates/ui/src/artifacts/tab.rs:1535`: `.border_color(theme.border)`
- `crates/ui/src/artifacts/tab.rs:1569`: `.rounded(px(6.0))`
- `crates/ui/src/artifacts/tab.rs:1570`: `.border_1()`
- `crates/ui/src/artifacts/tab.rs:1571`: `.border_color(theme.warning)`
- `crates/ui/src/artifacts/tab.rs:1629`: `.bg(theme.bg);`
- `crates/ui/src/artifacts/tab.rs:1667`: `.border_b_1()`
- `crates/ui/src/artifacts/tab.rs:1668`: `.border_color(theme.border)`
- `crates/ui/src/artifacts/tab.rs:1758`: `.rounded(px(3.0))`
- `crates/ui/src/artifacts/tab.rs:1759`: `.border_1()`
- `crates/ui/src/artifacts/tab.rs:1760`: `.border_color(theme.border)`

### Browser and artifacts remaining fixed typography

- `crates/ui/src/browser/fragment_view.rs:279`: stays literal, in baseline: fixed content text size, shared across styles and layouts.
- `crates/ui/src/browser/fragment_view.rs:287`: stays literal, in baseline: fixed content text size, shared across styles and layouts.
- `crates/ui/src/browser/fragment_view.rs:365`: stays literal, in baseline: fixed content text size, shared across styles and layouts.
- `crates/ui/src/browser/fragment_view.rs:373`: stays literal, in baseline: fixed content text size, shared across styles and layouts.
- `crates/ui/src/browser/fragment_view.rs:414`: stays literal, in baseline: fixed content text size, shared across styles and layouts.
- `crates/ui/src/browser/view.rs:115`: stays literal, in baseline: fixed content text size, shared across styles and layouts.
- `crates/ui/src/browser/view.rs:143`: stays literal, in baseline: fixed content text size, shared across styles and layouts.
- `crates/ui/src/browser/view.rs:151`: stays literal, in baseline: fixed content text size, shared across styles and layouts.
- `crates/ui/src/browser/view.rs:200`: stays literal, in baseline: fixed content text size, shared across styles and layouts.
- `crates/ui/src/browser/view.rs:210`: stays literal, in baseline: fixed content text size, shared across styles and layouts.
- `crates/ui/src/browser/view.rs:270`: stays literal, in baseline: fixed content text size, shared across styles and layouts.
- `crates/ui/src/browser/view.rs:278`: stays literal, in baseline: fixed content text size, shared across styles and layouts.
- `crates/ui/src/browser/view.rs:300`: stays literal, in baseline: fixed content text size, shared across styles and layouts.
- `crates/ui/src/browser/view.rs:573`: stays literal, in baseline: fixed content text size, shared across styles and layouts.
- `crates/ui/src/browser/view.rs:575`: stays literal, in baseline: fixed content text size, shared across styles and layouts.
- `crates/ui/src/browser/view.rs:579`: stays literal, in baseline: fixed content text size, shared across styles and layouts.
- `crates/ui/src/artifacts/cards.rs:119`: stays literal, in baseline: fixed content text size, shared across styles and layouts.
- `crates/ui/src/artifacts/cards.rs:126`: stays literal, in baseline: fixed content text size, shared across styles and layouts.
- `crates/ui/src/artifacts/plan_linear.rs:207`: stays literal, in baseline: fixed content text size, shared across styles and layouts.
- `crates/ui/src/artifacts/tab/comments_view.rs:513`: stays literal, in baseline: fixed content text size, shared across styles and layouts.
- `crates/ui/src/artifacts/tab/comments_view.rs:551`: stays literal, in baseline: fixed content text size, shared across styles and layouts.
- `crates/ui/src/artifacts/tab/comments_view.rs:577`: stays literal, in baseline: fixed content text size, shared across styles and layouts.
- `crates/ui/src/artifacts/tab/comments_view.rs:644`: stays literal, in baseline: fixed content text size, shared across styles and layouts.
- `crates/ui/src/artifacts/tab/comments_view.rs:654`: stays literal, in baseline: fixed content text size, shared across styles and layouts.
- `crates/ui/src/artifacts/tab/comments_view.rs:663`: stays literal, in baseline: fixed content text size, shared across styles and layouts.
- `crates/ui/src/artifacts/tab/comments_view.rs:670`: stays literal, in baseline: fixed content text size, shared across styles and layouts.
- `crates/ui/src/artifacts/tab/compare_view.rs:1077`: stays literal, in baseline: fixed content text size, shared across styles and layouts.
- `crates/ui/src/artifacts/tab/compare_view.rs:1306`: stays literal, in baseline: fixed content text size, shared across styles and layouts.
- `crates/ui/src/artifacts/tab/compare_view.rs:1375`: stays literal, in baseline: fixed content text size, shared across styles and layouts.
- `crates/ui/src/artifacts/tab/compare_view.rs:1427`: stays literal, in baseline: fixed content text size, shared across styles and layouts.
- `crates/ui/src/artifacts/tab/compare_view.rs:1470`: stays literal, in baseline: fixed content text size, shared across styles and layouts.
- `crates/ui/src/artifacts/tab/compare_view.rs:1511`: stays literal, in baseline: fixed content text size, shared across styles and layouts.
- `crates/ui/src/artifacts/tab/find_view.rs:793`: stays literal, in baseline: fixed content text size, shared across styles and layouts.
- `crates/ui/src/artifacts/tab/find_view.rs:916`: stays literal, in baseline: fixed content text size, shared across styles and layouts.
- `crates/ui/src/artifacts/tab/options_view.rs:642`: stays literal, in baseline: fixed content text size, shared across styles and layouts.
- `crates/ui/src/artifacts/tab/options_view.rs:695`: stays literal, in baseline: fixed content text size, shared across styles and layouts.
- `crates/ui/src/artifacts/tab/plan_view.rs:67`: stays literal, in baseline: fixed content text size, shared across styles and layouts.
- `crates/ui/src/artifacts/tab.rs:968`: stays literal, in baseline: fixed content text size, shared across styles and layouts.
- `crates/ui/src/artifacts/tab.rs:996`: stays literal, in baseline: fixed content text size, shared across styles and layouts.
- `crates/ui/src/artifacts/tab.rs:1211`: stays literal, in baseline: fixed content text size, shared across styles and layouts.
- `crates/ui/src/artifacts/tab.rs:1283`: stays literal, in baseline: fixed content text size, shared across styles and layouts.
- `crates/ui/src/artifacts/tab.rs:1307`: stays literal, in baseline: fixed content text size, shared across styles and layouts.
- `crates/ui/src/artifacts/tab.rs:1348`: stays literal, in baseline: fixed content text size, shared across styles and layouts.
- `crates/ui/src/artifacts/tab.rs:1384`: stays literal, in baseline: fixed content text size, shared across styles and layouts.
- `crates/ui/src/artifacts/tab.rs:1393`: stays literal, in baseline: fixed content text size, shared across styles and layouts.
- `crates/ui/src/artifacts/tab.rs:1404`: stays literal, in baseline: fixed content text size, shared across styles and layouts.
- `crates/ui/src/artifacts/tab.rs:1424`: stays literal, in baseline: fixed content text size, shared across styles and layouts.
- `crates/ui/src/artifacts/tab.rs:1432`: stays literal, in baseline: fixed content text size, shared across styles and layouts.
- `crates/ui/src/artifacts/tab.rs:1441`: stays literal, in baseline: fixed content text size, shared across styles and layouts.
- `crates/ui/src/artifacts/tab.rs:1483`: stays literal, in baseline: fixed content text size, shared across styles and layouts.
- `crates/ui/src/artifacts/tab.rs:1492`: stays literal, in baseline: fixed content text size, shared across styles and layouts.
- `crates/ui/src/artifacts/tab.rs:1517`: stays literal, in baseline: fixed content text size, shared across styles and layouts.
- `crates/ui/src/artifacts/tab.rs:1523`: stays literal, in baseline: fixed content text size, shared across styles and layouts.
- `crates/ui/src/artifacts/tab.rs:1531`: stays literal, in baseline: fixed content text size, shared across styles and layouts.
- `crates/ui/src/artifacts/tab.rs:1582`: stays literal, in baseline: fixed content text size, shared across styles and layouts.
- `crates/ui/src/artifacts/tab.rs:1604`: stays literal, in baseline: fixed content text size, shared across styles and layouts.

## Settings and appearance audit (in progress)
The index below is a mechanical inventory, including test and non-presentation matches. Each location needs classification during migration. Settings navigation chrome belongs to shell.
### Explicit style and layout references
- `crates/ui/src/settings.rs:756`: `pub ui_style: crate::ui_style::UiStyleId,`
- `crates/ui/src/settings.rs:757`: `pub ui_layout: crate::ui_style::UiLayout,`
- `crates/ui/src/settings.rs:874`: `ui_style: crate::ui_style::UiStyleId::default(),`
- `crates/ui/src/settings.rs:875`: `ui_layout: crate::ui_style::UiLayout::default(),`
- `crates/ui/src/settings.rs:3017`: `ui_style: crate::ui_style::UiStyleId::Default,`
- `crates/ui/src/settings.rs:3018`: `ui_layout: crate::ui_style::UiLayout::Compact,`
- `crates/ui/src/settings/appearance.rs:2997`: `let style_cards = crate::ui_style::UiStyleId::ALL`
- `crates/ui/src/settings/appearance.rs:3031`: `ui_settings.ui_style == id,`
- `crates/ui/src/settings/appearance.rs:3036`: `crate::ui_style::set_style(id, window, cx);`
- `crates/ui/src/settings/appearance.rs:3042`: `let layout_cards = crate::ui_style::UiLayout::ALL.iter().copied().map(|layout| {`
- `crates/ui/src/settings/appearance.rs:3045`: `crate::ui_style::UiLayout::Minimal => ("Current spacing", 4.0),`
- `crates/ui/src/settings/appearance.rs:3046`: `crate::ui_style::UiLayout::Compact => ("More on screen", 2.0),`
- `crates/ui/src/settings/appearance.rs:3047`: `crate::ui_style::UiLayout::Guided => ("More labels and guidance", 6.0),`
- `crates/ui/src/settings/appearance.rs:3057`: `crate::ui_style::set_layout(layout, window, cx);`
- `crates/ui/src/appearance.rs:107`: `let style = crate::ui_style::current(cx);`
- `crates/ui/src/appearance.rs:145`: `_style: crate::ui_style::UiStyle,`
- `crates/ui/src/appearance.rs:150`: `fn effective_accent(accent: AccentSelection, _style: crate::ui_style::UiStyle) -> AccentSelection {`
- `crates/ui/src/appearance.rs:156`: `style: crate::ui_style::UiStyle,`
- `crates/ui/src/appearance.rs:159`: `crate::ui_style::SurfaceModel::ThemeDefault => surface,`
- `crates/ui/src/appearance.rs:160`: `crate::ui_style::SurfaceModel::Opaque => SurfacePreference::Opaque,`
- `crates/ui/src/appearance.rs:161`: `crate::ui_style::SurfaceModel::Glass => SurfacePreference::Frosted,`
- `crates/ui/src/appearance.rs:169`: `let style = crate::ui_style::current(cx);`

### Inline presentation candidates

Locations are line numbers at the audit commit; each includes a numeric text size, radius, or border call. Some are preview artwork, text formatting, or tests, and require individual review.

- `crates/ui/src/settings/accounts.rs`: 355, 391, 446, 643, 701, 721, 1039, 1143, 1168, 1169, 1197, 1203, 1223, 1276, 1323, 1330, 1409, 1421, 1469, 1634, 1676, 1724, 1744, 1806, 1848
- `crates/ui/src/settings/appearance.rs`: 395, 423, 449, 1219, 1269, 1270, 1277, 1309, 1310, 1317, 1350, 1376, 1377, 1452, 1454, 1473, 1474, 1479, 1498, 1499, 1508, 1509, 1518, 1530, 1540, 1556, 1557, 1567, 1573, 1576, 1600, 1601, 1605, 1658, 1668, 1675, 1682, 1685, 1704, 1705, 1803, 1866, 1867, 1973, 1974, 2052, 2053, 2093, 2214, 2215, 2241, 2256, 2266, 2275, 2346, 2379, 2380, 2404, 2405, 2442, 2449, 2500, 2502, 2517, 2535, 2540, 2577, 2578, 2717, 2789, 2929, 2949, 3006, 3016, 3022, 3049, 3051, 3052, 3134, 3246, 3248, 3253, 3438, 3553
- `crates/ui/src/settings/appshots.rs`: 26, 91, 92, 103, 318, 332
- `crates/ui/src/settings/archived.rs`: 173, 189, 190, 217, 225, 240, 268, 269, 271, 314, 320
- `crates/ui/src/settings/devices.rs`: 351, 385, 427
- `crates/ui/src/settings/files.rs`: 92, 93, 104
- `crates/ui/src/settings/harnesses.rs`: 799, 834, 890, 1004, 1005
- `crates/ui/src/settings/shortcuts.rs`: 298, 306, 352, 378, 379, 383, 656, 685, 707, 724, 729, 737, 767, 912
- `crates/ui/src/settings/widgets.rs`: 177, 185, 196, 256, 258, 267, 302, 303, 322, 339, 398, 399, 402, 428, 429, 432

### Settings migration status

The original candidate index above records audit-commit locations. Explicit style and layout presentation decisions now reside in `components/settings_parts.rs`; the remaining `UiStyleId` / `UiLayout` uses in screens enumerate choices or persist settings, and the test constants are fixtures. Navigation remains in `shell_parts.rs`.

The following remaining ratchet hits are fixed-content typography or miniature artwork, not interactive control geometry. Line numbers below refer to this migration revision.

- `crates/ui/src/settings/accounts.rs` text sizes at 375, 430, 627, 685, 705, 1023, 1170, 1190, 1243, 1290, 1297, 1376, 1388, 1601, 1643, 1691, 1711, 1773, 1815: stays literal, in baseline: content and status text retain the same metrics across styles.
- `crates/ui/src/settings/appearance.rs` text sizes at 1346, 1358, 1742, 1869, 1940, 1996, 2003, 2065, 2238, 2310, 2450, 2470, 3017: stays literal, in baseline: content and status text retain the same metrics across styles.
- `crates/ui/src/settings/appshots.rs` text sizes at 26: stays literal, in baseline: content and status text retain the same metrics across styles.
- `crates/ui/src/settings/archived.rs` text sizes at 195, 203, 218, 275, 281: stays literal, in baseline: content and status text retain the same metrics across styles.
- `crates/ui/src/settings/devices.rs` text sizes at 351, 427: stays literal, in baseline: content and status text retain the same metrics across styles.
- `crates/ui/src/settings/harnesses.rs` text sizes at 818, 874: stays literal, in baseline: content and status text retain the same metrics across styles.
- `crates/ui/src/settings/shortcuts.rs` text sizes at 298, 306, 352, 628, 849: stays literal, in baseline: content and status text retain the same metrics across styles.
- `crates/ui/src/settings/accounts.rs` radii at 1436: stays literal, in baseline: loading skeleton placeholders preserve their geometry.
- `crates/ui/src/settings/appearance.rs` radii at 1292, 1326, 1336, 1368, 1384, 1395, 1401, 1404: stays literal, in baseline: thumbnail artwork models the selected theme.
