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

Each slice must audit its own inline call sites and remove screen-owned style
and layout branches after shared components support the existing treatments.
The candidate index is not a substitute for that audit.

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
