use super::*;
use std::cell::Cell;
use std::collections::{BTreeSet, HashMap};
use std::rc::Rc;

const T0: i64 = 1_800_000_000_000;
const DAY: i64 = 24 * 60 * 60 * 1000;

fn ledger() -> (tempfile::TempDir, ModelFitLedger) {
    let dir = tempfile::tempdir().unwrap();
    let ledger = ModelFitLedger::open(dir.path()).unwrap();
    (dir, ledger)
}

fn class() -> TaskClass {
    TaskClass {
        role: TaskRole::Implementer,
        domain: TaskDomain::EngineConcurrencyState,
        complexity: ComplexityTier::Standard,
    }
}

fn model(name: &str) -> ModelKey {
    ModelKey::new("claudeCode", name)
}

fn outcome(id: &str, name: &str, tier: ModelTier, result: TaskResult) -> Outcome {
    Outcome {
        outcome_id: id.into(),
        task_id: format!("task-{id}"),
        task_class: class(),
        model: model(name),
        version: "1.0".into(),
        tier,
        result,
        canary: false,
        peak_context_tokens: Some(10_000),
        context_window_tokens: Some(200_000),
    }
}

const PASS: TaskResult = TaskResult::CleanPass;
const FAILED: TaskResult = TaskResult::Failed(FailureKind::GateCompileOrTestFail);

fn small(ledger: &ModelFitLedger, id: &str, name: &str, result: TaskResult, at: i64) -> Recorded {
    ledger
        .record(&outcome(id, name, ModelTier::Small, result), at)
        .unwrap()
}

fn fit(ledger: &ModelFitLedger, name: &str, at: i64) -> FitState {
    ledger.fit(&class(), &model(name), at).unwrap().unwrap()
}

/// Demote two Small models, which raises the floor to Medium until `T0 + 7d`.
fn raise_floor_to_medium(ledger: &ModelFitLedger) {
    for name in ["haiku", "flash"] {
        small(ledger, &format!("{name}-1"), name, FAILED, T0);
        small(ledger, &format!("{name}-2"), name, FAILED, T0);
    }
    assert_eq!(ledger.floor(&class()).unwrap().min_tier, ModelTier::Medium);
}

#[test]
fn domain_comes_from_the_hardest_path() {
    for (paths, domain) in [
        (vec!["docs/internal/model-fit.md"], TaskDomain::DocsConfig),
        (vec!["README.md", "Cargo.toml"], TaskDomain::DocsConfig),
        (vec!["crates/ui/src/overview.rs"], TaskDomain::UiGpui),
        (vec!["crates/mcp/src/tools.rs"], TaskDomain::McpOrCli),
        (vec!["apps/mast/src/main.rs"], TaskDomain::McpOrCli),
        (vec!["apps/mast-gallery/src/main.rs"], TaskDomain::UiGpui),
        (
            vec!["crates/harness/src/acp.rs"],
            TaskDomain::HarnessProtocol,
        ),
        (vec!["crates/proto/**"], TaskDomain::ProtoOrDocSchema),
        (
            vec!["./crates/engine/src/model_fit/**", "docs/changelog.d/a.md"],
            TaskDomain::EngineConcurrencyState,
        ),
        (
            vec!["crates/ui/src/a.rs", "crates/proto/src/b.rs"],
            TaskDomain::ProtoOrDocSchema,
        ),
        (
            vec!["crates/syntax/src/lib.rs"],
            TaskDomain::MechanicalRefactor,
        ),
        (vec!["crates/enginex/a.rs"], TaskDomain::MechanicalRefactor),
        (vec![], TaskDomain::MechanicalRefactor),
        (vec!["**"], TaskDomain::EngineConcurrencyState),
        (vec!["crates/**"], TaskDomain::EngineConcurrencyState),
        (vec!["crates/*/src/**"], TaskDomain::EngineConcurrencyState),
        (
            vec!["crates/{ui,mcp}/**"],
            TaskDomain::EngineConcurrencyState,
        ),
        (vec!["crates/ui/**"], TaskDomain::UiGpui),
        (vec!["crates/ui*/**"], TaskDomain::UiGpui),
        (vec!["crates/pro*"], TaskDomain::ProtoOrDocSchema),
        (vec!["apps/**"], TaskDomain::UiGpui),
        (vec!["crates/syntax/**"], TaskDomain::MechanicalRefactor),
        (vec!["docs/**"], TaskDomain::DocsConfig),
        (vec!["**/*.md"], TaskDomain::DocsConfig),
    ] {
        assert_eq!(classify_domain(&paths), domain, "{paths:?}");
    }
    let class = task_class(TaskRole::DocWriter, ComplexityTier::Trivial, ["docs/a.md"]);
    assert_eq!(class.role, TaskRole::DocWriter);
    assert_eq!(class.domain, TaskDomain::DocsConfig);
    assert_eq!(class.complexity, ComplexityTier::Trivial);
}

#[test]
fn infrastructure_failures_never_count() {
    let (_dir, ledger) = ledger();
    let excluded = [
        FailureKind::QuotaOrRateLimit,
        FailureKind::AuthExpiry,
        FailureKind::DiskHold,
        FailureKind::PeerDisconnect,
    ];
    for (index, kind) in excluded.into_iter().cycle().take(12).enumerate() {
        let recorded = small(
            &ledger,
            &index.to_string(),
            "haiku",
            TaskResult::Failed(kind),
            T0,
        );
        assert_eq!(recorded.attribution, Attribution::Excluded);
        assert_eq!(recorded.demoted_until_ms, None);
    }
    assert_eq!(ledger.fit(&class(), &model("haiku"), T0).unwrap(), None);
    assert_eq!(ledger.outcome_count().unwrap(), 12);
}

#[test]
fn excluded_failure_does_not_break_a_failure_streak() {
    let (_dir, ledger) = ledger();
    small(&ledger, "1", "haiku", FAILED, T0);
    small(
        &ledger,
        "2",
        "haiku",
        TaskResult::Failed(FailureKind::AuthExpiry),
        T0,
    );
    assert!(
        small(&ledger, "3", "haiku", FAILED, T0)
            .demoted_until_ms
            .is_some()
    );
}

#[test]
fn context_overload_does_not_demote() {
    let (_dir, ledger) = ledger();
    for index in 0..6 {
        let mut overloaded = outcome(&index.to_string(), "haiku", ModelTier::Small, FAILED);
        overloaded.peak_context_tokens = Some(121_000);
        let recorded = ledger.record(&overloaded, T0).unwrap();
        assert_eq!(recorded.attribution, Attribution::ContextOverload);
        assert_eq!(recorded.demoted_until_ms, None);
    }
    assert_eq!(ledger.fit(&class(), &model("haiku"), T0).unwrap(), None);
}

#[test]
fn context_ceiling_depends_on_the_tier() {
    let at = |tier, peak| context_overloaded(tier, Some(peak), Some(100_000));
    for (tier, ceiling) in [
        (ModelTier::Small, 60_000),
        (ModelTier::Medium, 78_000),
        (ModelTier::Large, 78_000),
    ] {
        assert!(!at(tier, ceiling - 1), "{tier:?}");
        assert!(!at(tier, ceiling), "{tier:?}");
        assert!(at(tier, ceiling + 1), "{tier:?}");
    }
    assert!(!context_overloaded(ModelTier::Small, None, Some(100_000)));
    assert!(!context_overloaded(ModelTier::Small, Some(99_000), None));
    assert!(!context_overloaded(ModelTier::Small, Some(99_000), Some(0)));
}

#[test]
fn review_rejects_count_from_the_second_on_the_same_task() {
    let (_dir, ledger) = ledger();
    let reject = |id: &str, task: &str| {
        let mut rejected = outcome(
            id,
            "haiku",
            ModelTier::Small,
            TaskResult::Failed(FailureKind::SubstantiveReviewReject),
        );
        rejected.task_id = task.into();
        ledger.record(&rejected, T0).unwrap().attribution
    };
    assert_eq!(reject("1", "a"), Attribution::BelowThreshold);
    assert_eq!(reject("2", "b"), Attribution::BelowThreshold);
    assert_eq!(reject("3", "a"), Attribution::ReasoningInadequacy);
    assert_eq!(fit(&ledger, "haiku", T0).trials, 1);
}

#[test]
fn review_rejects_count_across_a_change_of_task_class() {
    let (_dir, ledger) = ledger();
    let mut reject = outcome(
        "1",
        "haiku",
        ModelTier::Small,
        TaskResult::Failed(FailureKind::SubstantiveReviewReject),
    );
    reject.task_id = "a".into();
    let first = ledger.record(&reject, T0).unwrap();
    assert_eq!(first.attribution, Attribution::BelowThreshold);
    // The fix touched the wire types, so the task is now classed differently.
    reject.outcome_id = "2".into();
    reject.task_class.domain = TaskDomain::ProtoOrDocSchema;
    let second = ledger.record(&reject, T0).unwrap();
    assert_eq!(second.attribution, Attribution::ReasoningInadequacy);
}

#[test]
fn a_failure_rate_just_under_the_threshold_does_not_demote() {
    // Passes, then failures that are never consecutive. The last failure takes
    // the rate to 0.3 + 0.7 * previous.
    let rate_after = |passes: usize| {
        let (_dir, ledger) = ledger();
        small(&ledger, "f0", "haiku", FAILED, T0);
        for index in 0..passes {
            small(&ledger, &format!("p{index}"), "haiku", PASS, T0);
        }
        let before = fit(&ledger, "haiku", T0);
        assert!(!before.is_demoted(T0));
        let last = small(&ledger, "f1", "haiku", FAILED, T0);
        (before, last, fit(&ledger, "haiku", T0))
    };
    // After three passes the rate is 0.2333; the failure makes it 0.4633.
    let (before, last, _) = rate_after(3);
    assert!((before.ewma_fail_rate - 0.7 / 3.0).abs() < 1e-9);
    assert!(last.demoted_until_ms.is_some());
    // A fourth pass leaves 0.1633; the failure makes it only 0.4143.
    let (before, last, after) = rate_after(4);
    assert!((before.ewma_fail_rate - 0.49 / 3.0).abs() < 1e-9);
    assert_eq!(last.demoted_until_ms, None);
    assert!((after.ewma_fail_rate - (0.3 + 0.7 * 0.49 / 3.0)).abs() < 1e-9);
}

#[test]
fn two_trials_are_too_few_to_demote_on_rate() {
    let (_dir, ledger) = ledger();
    small(&ledger, "1", "haiku", PASS, T0);
    let second = small(&ledger, "2", "haiku", FAILED, T0);
    assert_eq!(second.demoted_until_ms, None);
    let state = fit(&ledger, "haiku", T0);
    assert_eq!(state.trials, 2);
    assert_eq!(state.ewma_fail_rate, 0.5);
}

#[test]
fn old_failures_are_not_a_streak() {
    let (_dir, ledger) = ledger();
    small(&ledger, "1", "haiku", FAILED, T0);
    let later = T0 + 140 * DAY;
    assert_eq!(fit(&ledger, "haiku", later).trials, 0);
    assert_eq!(fit(&ledger, "haiku", later).consecutive_failures, 0);
    let second = small(&ledger, "2", "haiku", FAILED, later);
    assert_eq!(second.demoted_until_ms, None);
    let state = fit(&ledger, "haiku", later);
    assert_eq!((state.trials, state.consecutive_failures), (1, 1));
}

#[test]
fn a_streak_lasts_one_half_life() {
    let (_dir, ledger) = ledger();
    small(&ledger, "1", "haiku", FAILED, T0);
    let inside = small(&ledger, "2", "haiku", FAILED, T0 + 14 * DAY);
    assert!(inside.demoted_until_ms.is_some());
    small(&ledger, "3", "flash", FAILED, T0);
    let outside = small(&ledger, "4", "flash", FAILED, T0 + 14 * DAY + 1);
    assert_eq!(outside.demoted_until_ms, None);
}

#[test]
fn only_recent_trials_count_towards_the_rate_rule() {
    let (_dir, ledger) = ledger();
    // Never more than a half-life apart, so the history is never dropped whole.
    for index in 0..4 {
        small(&ledger, &format!("p{index}"), "haiku", PASS, T0);
    }
    small(&ledger, "p4", "haiku", PASS, T0 + 10 * DAY);
    small(&ledger, "f", "haiku", FAILED, T0 + 20 * DAY);
    assert_eq!(fit(&ledger, "haiku", T0 + 20 * DAY).trials, 2);
}

#[test]
fn late_results_during_a_demotion_are_not_evidence() {
    let (_dir, ledger) = ledger();
    small(&ledger, "1", "haiku", FAILED, T0);
    small(&ledger, "2", "haiku", FAILED, T0);
    for id in ["3", "4", "5"] {
        let late = small(&ledger, id, "haiku", FAILED, T0 + 6 * DAY);
        assert_eq!(late.demoted_until_ms, None);
    }
    let after = T0 + 7 * DAY;
    assert_eq!(
        small(&ledger, "6", "haiku", FAILED, after).demoted_until_ms,
        None
    );
    assert_eq!(fit(&ledger, "haiku", after).trials, 1);
}

#[test]
fn replaying_an_outcome_changes_nothing() {
    let (_dir, ledger) = ledger();
    small(&ledger, "1", "haiku", FAILED, T0);
    let replay = small(&ledger, "1", "haiku", FAILED, T0 + 1);
    assert!(replay.duplicate);
    assert_eq!(replay.attribution, Attribution::ReasoningInadequacy);
    assert_eq!(fit(&ledger, "haiku", T0).trials, 1);
    assert_eq!(ledger.outcome_count().unwrap(), 1);
}

#[test]
fn two_consecutive_failures_demote_and_repeats_double_to_thirty_days() {
    let (_dir, ledger) = ledger();
    let mut now = T0;
    for (round, days) in [7, 14, 28, 30, 30].into_iter().enumerate() {
        let first = small(&ledger, &format!("{round}-a"), "haiku", FAILED, now);
        assert_eq!(first.demoted_until_ms, None, "round {round}");
        let second = small(&ledger, &format!("{round}-b"), "haiku", FAILED, now);
        assert_eq!(second.demoted_until_ms, Some(now + days * DAY));
        assert!(fit(&ledger, "haiku", now).is_demoted(now + days * DAY - 1));
        assert!(!fit(&ledger, "haiku", now).is_demoted(now + days * DAY));
        now += days * DAY;
    }
}

#[test]
fn a_pass_breaks_the_failure_streak() {
    let (_dir, ledger) = ledger();
    small(&ledger, "1", "haiku", PASS, T0);
    small(&ledger, "2", "haiku", PASS, T0);
    small(&ledger, "3", "haiku", FAILED, T0);
    small(&ledger, "4", "haiku", PASS, T0);
    assert!(!fit(&ledger, "haiku", T0).is_demoted(T0));
}

#[test]
fn high_failure_rate_over_three_trials_demotes() {
    let (_dir, ledger) = ledger();
    small(&ledger, "1", "haiku", FAILED, T0);
    small(&ledger, "2", "haiku", PASS, T0);
    // 2 of 3 failed, never twice in a row.
    let third = small(&ledger, "3", "haiku", FAILED, T0);
    assert_eq!(third.demoted_until_ms, Some(T0 + 7 * DAY));
}

#[test]
fn low_failure_rate_does_not_demote() {
    let (_dir, ledger) = ledger();
    small(&ledger, "1", "haiku", PASS, T0);
    small(&ledger, "2", "haiku", PASS, T0);
    let third = small(&ledger, "3", "haiku", FAILED, T0);
    assert_eq!(third.demoted_until_ms, None);
    assert!((fit(&ledger, "haiku", T0).ewma_fail_rate - 1.0 / 3.0).abs() < 1e-9);
}

#[test]
fn two_demoted_models_raise_the_class_floor_only() {
    let (_dir, ledger) = ledger();
    small(&ledger, "1", "haiku", FAILED, T0);
    assert_eq!(
        small(&ledger, "2", "haiku", FAILED, T0).floor_moved_to,
        None
    );
    assert_eq!(ledger.floor(&class()).unwrap(), FloorState::default());
    small(&ledger, "3", "flash", FAILED, T0);
    assert_eq!(
        small(&ledger, "4", "flash", FAILED, T0).floor_moved_to,
        Some(ModelTier::Medium)
    );
    assert_eq!(
        ledger.floor(&class()).unwrap().cooldown_until_ms,
        T0 + 7 * DAY
    );
    let other = TaskClass {
        domain: TaskDomain::DocsConfig,
        ..class()
    };
    assert_eq!(ledger.floor(&other).unwrap(), FloorState::default());
}

#[test]
fn expired_demotions_do_not_raise_the_floor() {
    let (_dir, ledger) = ledger();
    small(&ledger, "1", "haiku", FAILED, T0);
    small(&ledger, "2", "haiku", FAILED, T0);
    small(&ledger, "3", "flash", FAILED, T0 + 8 * DAY);
    small(&ledger, "4", "flash", FAILED, T0 + 8 * DAY);
    assert_eq!(ledger.floor(&class()).unwrap().min_tier, ModelTier::Small);
}

#[test]
fn one_demotion_at_each_of_two_tiers_does_not_raise_the_floor() {
    let (_dir, ledger) = ledger();
    small(&ledger, "1", "haiku", FAILED, T0);
    small(&ledger, "2", "haiku", FAILED, T0);
    for id in ["3", "4"] {
        ledger
            .record(&outcome(id, "sonnet", ModelTier::Medium, FAILED), T0)
            .unwrap();
    }
    assert_eq!(ledger.floor(&class()).unwrap(), FloorState::default());
}

#[test]
fn two_demoted_medium_models_raise_the_floor_to_large() {
    let (_dir, ledger) = ledger();
    raise_floor_to_medium(&ledger);
    let mut moved = Vec::new();
    for name in ["sonnet", "codex"] {
        for attempt in ["1", "2"] {
            let id = format!("{name}-{attempt}");
            let failed = outcome(&id, name, ModelTier::Medium, FAILED);
            moved.extend(ledger.record(&failed, T0).unwrap().floor_moved_to);
        }
    }
    assert_eq!(moved, [ModelTier::Large]);
    assert_eq!(ledger.floor(&class()).unwrap().min_tier, ModelTier::Large);
}

#[test]
fn failure_rate_halves_every_fourteen_days() {
    let (_dir, ledger) = ledger();
    small(&ledger, "1", "haiku", FAILED, T0);
    assert_eq!(fit(&ledger, "haiku", T0).ewma_fail_rate, 1.0);
    assert!((fit(&ledger, "haiku", T0 + 14 * DAY).ewma_fail_rate - 0.5).abs() < 1e-9);
    assert!((fit(&ledger, "haiku", T0 + 28 * DAY).ewma_fail_rate - 0.25).abs() < 1e-9);
    // A clock that went backwards must not inflate the rate.
    assert_eq!(fit(&ledger, "haiku", T0 - DAY).ewma_fail_rate, 1.0);
}

#[test]
fn a_new_version_halves_evidence_and_remaining_demotion_once() {
    let (_dir, ledger) = ledger();
    for index in 0..4 {
        small(&ledger, &format!("p{index}"), "haiku", PASS, T0);
    }
    small(&ledger, "f", "haiku", FAILED, T0);
    small(&ledger, "x1", "flash", FAILED, T0);
    small(&ledger, "x2", "flash", FAILED, T0);

    assert!(!ledger.observe_version(&model("haiku"), "1.0", T0).unwrap());
    assert!(!ledger.observe_version(&model("haiku"), "", T0).unwrap());
    assert!(ledger.observe_version(&model("haiku"), "2.0", T0).unwrap());
    let haiku = fit(&ledger, "haiku", T0);
    assert_eq!(haiku.trials, 2);
    assert!((haiku.ewma_fail_rate - 0.15).abs() < 1e-9);
    assert_eq!(haiku.consecutive_failures, 0);
    // Going back and forth between known versions resets nothing more.
    assert!(!ledger.observe_version(&model("haiku"), "1.0", T0).unwrap());
    assert!(!ledger.observe_version(&model("haiku"), "2.0", T0).unwrap());
    assert_eq!(fit(&ledger, "haiku", T0), haiku);

    assert!(
        ledger
            .observe_version(&model("flash"), "2.0", T0 + DAY)
            .unwrap()
    );
    let flash = fit(&ledger, "flash", T0 + DAY);
    assert_eq!(flash.demoted_until_ms, T0 + 4 * DAY);
    assert_eq!(flash.demotion_level, 0);
}

#[test]
fn a_version_reset_shortens_the_floor_cooldown_with_the_demotions() {
    let (_dir, ledger) = ledger();
    raise_floor_to_medium(&ledger);
    let cooldown = || ledger.floor(&class()).unwrap().cooldown_until_ms;
    for name in ["haiku", "flash"] {
        ledger.observe_version(&model(name), "1.0", T0).unwrap();
    }
    let bumped = T0 + DAY;
    ledger
        .observe_version(&model("haiku"), "2.0", bumped)
        .unwrap();
    // The other raiser is still demoted until day 7.
    assert_eq!(cooldown(), T0 + 7 * DAY);
    ledger
        .observe_version(&model("flash"), "2.0", bumped)
        .unwrap();
    assert_eq!(cooldown(), T0 + 4 * DAY);
    assert_eq!(fit(&ledger, "haiku", bumped).demoted_until_ms, T0 + 4 * DAY);
    assert_eq!(ledger.floor(&class()).unwrap().min_tier, ModelTier::Medium);
}

#[test]
fn a_cooldown_with_no_running_demotion_left_ends_at_once() {
    let (_dir, ledger) = ledger();
    raise_floor_to_medium(&ledger);
    for name in ["haiku", "flash"] {
        ledger.observe_version(&model(name), "1.0", T0).unwrap();
    }
    // One millisecond of each demotion is left, and half of it rounds to none.
    let bumped = T0 + 7 * DAY - 1;
    for name in ["haiku", "flash"] {
        ledger.observe_version(&model(name), "2.0", bumped).unwrap();
    }
    assert_eq!(ledger.floor(&class()).unwrap().cooldown_until_ms, bumped);
}

fn canary(ledger: &ModelFitLedger, id: &str, result: TaskResult) -> Recorded {
    let mut probe = outcome(id, "mini", ModelTier::Small, result);
    probe.canary = true;
    ledger.record(&probe, T0 + 8 * DAY).unwrap()
}

#[test]
fn two_clean_canary_passes_lower_the_floor() {
    let (_dir, ledger) = ledger();
    raise_floor_to_medium(&ledger);
    assert_eq!(canary(&ledger, "c1", PASS).floor_moved_to, None);
    assert_eq!(ledger.floor(&class()).unwrap().canary_passes, 1);
    // An infrastructure failure says nothing either way.
    canary(&ledger, "c2", TaskResult::Failed(FailureKind::DiskHold));
    // A replay is not a second pass.
    canary(&ledger, "c1", PASS);
    assert_eq!(ledger.floor(&class()).unwrap().canary_passes, 1);
    assert_eq!(
        canary(&ledger, "c3", PASS).floor_moved_to,
        Some(ModelTier::Small)
    );
    let floor = ledger.floor(&class()).unwrap();
    assert_eq!(floor.min_tier, ModelTier::Small);
    assert_eq!(floor.canary_passes, 0);
}

#[test]
fn a_failed_canary_restarts_the_count() {
    let (_dir, ledger) = ledger();
    raise_floor_to_medium(&ledger);
    canary(&ledger, "c1", PASS);
    canary(&ledger, "c2", FAILED);
    canary(&ledger, "c3", PASS);
    let floor = ledger.floor(&class()).unwrap();
    assert_eq!(floor.min_tier, ModelTier::Medium);
    assert_eq!(floor.canary_passes, 1);
}

#[test]
fn a_canary_that_is_not_clean_restarts_the_count() {
    let overloaded = {
        let mut probe = outcome("bad", "mini", ModelTier::Small, FAILED);
        probe.peak_context_tokens = Some(150_000);
        (probe, Attribution::ContextOverload)
    };
    let first_reject = (
        outcome(
            "bad",
            "mini",
            ModelTier::Small,
            TaskResult::Failed(FailureKind::SubstantiveReviewReject),
        ),
        Attribution::BelowThreshold,
    );
    for (mut probe, attribution) in [overloaded, first_reject] {
        let (_dir, ledger) = ledger();
        raise_floor_to_medium(&ledger);
        canary(&ledger, "c1", PASS);
        probe.canary = true;
        let recorded = ledger.record(&probe, T0 + 8 * DAY).unwrap();
        assert_eq!(recorded.attribution, attribution);
        assert_eq!(ledger.floor(&class()).unwrap().canary_passes, 0);
        assert_eq!(canary(&ledger, "c2", PASS).floor_moved_to, None);
        assert_eq!(ledger.floor(&class()).unwrap().min_tier, ModelTier::Medium);
    }
}

#[test]
fn a_pass_after_a_review_reject_is_not_a_clean_canary() {
    let (_dir, ledger) = ledger();
    raise_floor_to_medium(&ledger);
    let at = T0 + 8 * DAY;
    let mut attempt = outcome(
        "reject",
        "mini",
        ModelTier::Small,
        TaskResult::Failed(FailureKind::SubstantiveReviewReject),
    );
    attempt.task_id = "a".into();
    ledger.record(&attempt, at).unwrap();
    canary(&ledger, "c1", PASS);
    attempt.outcome_id = "fixed".into();
    attempt.result = PASS;
    attempt.canary = true;
    assert_eq!(ledger.record(&attempt, at).unwrap().floor_moved_to, None);
    let floor = ledger.floor(&class()).unwrap();
    assert_eq!(
        (floor.min_tier, floor.canary_passes),
        (ModelTier::Medium, 0)
    );
}

#[test]
fn only_canaries_just_below_the_floor_count() {
    let (_dir, ledger) = ledger();
    raise_floor_to_medium(&ledger);
    // Ordinary passes, and probes at the floor's own tier, change nothing.
    small(&ledger, "p1", "mini", PASS, T0 + 8 * DAY);
    for id in ["c1", "c2"] {
        let mut probe = outcome(id, "sonnet", ModelTier::Medium, PASS);
        probe.canary = true;
        ledger.record(&probe, T0 + 8 * DAY).unwrap();
    }
    let floor = ledger.floor(&class()).unwrap();
    assert_eq!(floor.min_tier, ModelTier::Medium);
    assert_eq!(floor.canary_passes, 0);
}

#[test]
fn store_survives_reopen() {
    let (dir, first) = ledger();
    raise_floor_to_medium(&first);
    first.observe_version(&model("haiku"), "1.0", T0).unwrap();
    let haiku = fit(&first, "haiku", T0);
    let floor = first.floor(&class()).unwrap();
    drop(first);

    let reopened = ModelFitLedger::open(dir.path()).unwrap();
    assert_eq!(fit(&reopened, "haiku", T0), haiku);
    assert_eq!(reopened.floor(&class()).unwrap(), floor);
    assert_eq!(reopened.outcome_count().unwrap(), 4);
    assert!(small(&reopened, "haiku-1", "haiku", FAILED, T0).duplicate);
    assert!(
        reopened
            .observe_version(&model("haiku"), "2.0", T0)
            .unwrap()
    );
}

#[test]
fn a_newer_schema_is_refused() {
    let (dir, first) = ledger();
    drop(first);
    rusqlite::Connection::open(dir.path().join("model-fit.sqlite3"))
        .unwrap()
        .pragma_update(None, "user_version", 99)
        .unwrap();
    assert!(ModelFitLedger::open(dir.path()).is_err());
}

#[derive(Clone)]
struct TestClock(Rc<Cell<i64>>);
impl Clock for TestClock {
    fn now_ms(&self) -> i64 {
        self.0.get()
    }
}

/// Returns one fixed value and counts the rolls.
struct FixedRng(f64, Rc<Cell<u32>>);
impl UniformRng for FixedRng {
    fn next_unit(&mut self) -> f64 {
        self.1.set(self.1.get() + 1);
        self.0
    }
}

#[derive(Default)]
struct Quota {
    empty: BTreeSet<ModelKey>,
    used: HashMap<ModelKey, f64>,
    usage_unknown: bool,
}
impl Headroom for Quota {
    fn headroom(&self, model: &ModelKey) -> f64 {
        if self.empty.contains(model) { 0.0 } else { 1.0 }
    }
    fn used_of_cap(&self, model: &ModelKey) -> Option<f64> {
        (!self.usage_unknown).then(|| self.used.get(model).copied().unwrap_or(0.1))
    }
}

fn candidate(name: &str, tier: ModelTier, token_cost: u64) -> Candidate {
    Candidate {
        model: model(name),
        version: "1.0".into(),
        tier,
        token_cost,
        capabilities: BTreeSet::from(["tools".to_owned()]),
        context_window_tokens: 200_000,
    }
}

fn candidates() -> Vec<Candidate> {
    vec![
        candidate("opus", ModelTier::Large, 1),
        candidate("sonnet", ModelTier::Medium, 30),
        candidate("codex", ModelTier::Medium, 20),
        candidate("haiku", ModelTier::Small, 9),
        candidate("mini", ModelTier::Small, 10),
    ]
}

fn request() -> RouteRequest {
    RouteRequest {
        task_class: class(),
        packet_tokens: 3_000,
        required_capabilities: BTreeSet::from(["tools".to_owned()]),
        ..Default::default()
    }
}

struct Rig {
    _dir: tempfile::TempDir,
    router: ModelRouter<TestClock, FixedRng>,
    clock: Rc<Cell<i64>>,
    rolls: Rc<Cell<u32>>,
}

fn rig(roll: f64) -> Rig {
    let (dir, ledger) = ledger();
    let clock = Rc::new(Cell::new(T0));
    let rolls = Rc::new(Cell::new(0));
    Rig {
        _dir: dir,
        router: ModelRouter::with_sources(
            ledger,
            TestClock(clock.clone()),
            FixedRng(roll, rolls.clone()),
        ),
        clock,
        rolls,
    }
}

fn pick(rig: &mut Rig, request: &RouteRequest, quota: &Quota) -> Selection {
    rig.router.select(request, &candidates(), quota).unwrap()
}

#[test]
fn router_picks_the_cheapest_tier_then_cost_in_any_order() {
    let mut rig = rig(0.0);
    let chosen = pick(&mut rig, &request(), &Quota::default());
    assert_eq!(chosen.model, model("haiku"));
    assert_eq!(chosen.tier, ModelTier::Small);
    assert!(!chosen.canary);
    let mut reversed = candidates();
    reversed.reverse();
    assert_eq!(
        rig.router
            .select(&request(), &reversed, &Quota::default())
            .unwrap(),
        chosen
    );
    // The smallest floor has nothing below it to probe.
    assert_eq!(rig.rolls.get(), 0);
}

#[test]
fn router_breaks_a_price_tie_by_model_in_either_order() {
    let mut rig = rig(0.99);
    let mut offered = vec![
        candidate("zeta", ModelTier::Small, 5),
        candidate("alpha", ModelTier::Small, 5),
    ];
    for _ in 0..2 {
        let chosen = rig
            .router
            .select(&request(), &offered, &Quota::default())
            .unwrap();
        assert_eq!(chosen.model, model("alpha"));
        offered.reverse();
    }
}

#[test]
fn router_respects_headroom_capabilities_context_and_minimum_tier() {
    let mut rig = rig(0.99);
    let quota = Quota {
        empty: BTreeSet::from([model("haiku")]),
        ..Default::default()
    };
    assert_eq!(pick(&mut rig, &request(), &quota).model, model("mini"));

    let mut offered = candidates();
    offered[1].capabilities.insert("vision".into());
    let mut needs_vision = request();
    needs_vision.required_capabilities.insert("vision".into());
    let chosen = rig.router.select(&needs_vision, &offered, &quota).unwrap();
    assert_eq!(chosen.model, model("sonnet"));

    let mut offered = candidates();
    offered[3].context_window_tokens = 2_999;
    offered[4].context_window_tokens = 3_000;
    let chosen = rig
        .router
        .select(&request(), &offered, &Quota::default())
        .unwrap();
    assert_eq!(chosen.model, model("mini"));

    let swapped_up = RouteRequest {
        min_tier: Some(tier_above(ModelTier::Small)),
        ..request()
    };
    assert_eq!(
        pick(&mut rig, &swapped_up, &Quota::default()).model,
        model("codex")
    );
}

#[test]
fn router_reports_why_nothing_is_eligible() {
    let mut rig = rig(0.99);
    small(rig.router.ledger(), "1", "haiku", FAILED, T0);
    small(rig.router.ledger(), "2", "haiku", FAILED, T0);
    let offered = vec![
        candidate("haiku", ModelTier::Small, 1),
        candidate("mini", ModelTier::Small, 2),
    ];
    let quota = Quota {
        empty: BTreeSet::from([model("mini")]),
        ..Default::default()
    };
    let Err(RouteError::NoEligibleModel(rejected)) =
        rig.router.select(&request(), &offered, &quota)
    else {
        panic!("expected no eligible model");
    };
    assert_eq!(
        rejected.iter().map(|r| r.reason).collect::<Vec<_>>(),
        [
            Rejection::Demoted {
                until_ms: T0 + 7 * DAY
            },
            Rejection::NoHeadroom
        ]
    );
}

#[test]
fn router_skips_a_demoted_model_until_the_demotion_ends() {
    let mut rig = rig(0.99);
    small(rig.router.ledger(), "1", "haiku", FAILED, T0);
    small(rig.router.ledger(), "2", "haiku", FAILED, T0);
    assert_eq!(
        pick(&mut rig, &request(), &Quota::default()).model,
        model("mini")
    );
    rig.clock.set(T0 + 7 * DAY);
    assert_eq!(
        pick(&mut rig, &request(), &Quota::default()).model,
        model("haiku")
    );
}

#[test]
fn router_lifts_a_demotion_early_for_a_new_version() {
    let mut rig = rig(0.99);
    small(rig.router.ledger(), "1", "haiku", FAILED, T0);
    small(rig.router.ledger(), "2", "haiku", FAILED, T0);
    rig.clock.set(T0 + 4 * DAY);
    let mut offered = candidates();
    offered[3].version = "2.0".into();
    // 3 days remain, halved from the moment the version is seen.
    let chosen = rig.router.select(&request(), &offered, &Quota::default());
    assert_eq!(chosen.unwrap().model, model("mini"));
    rig.clock.set(T0 + 4 * DAY + 3 * DAY / 2);
    let chosen = rig.router.select(&request(), &offered, &Quota::default());
    assert_eq!(chosen.unwrap().model, model("haiku"));
}

#[test]
fn router_probes_below_the_floor_after_cooldown() {
    let mut rig = rig(0.099);
    raise_floor_to_medium(rig.router.ledger());

    let during = pick(&mut rig, &request(), &Quota::default());
    assert_eq!(during.model, model("codex"));
    assert_eq!(during.floor, ModelTier::Medium);
    assert!(!during.canary);
    assert_eq!(rig.rolls.get(), 0);

    rig.clock.set(T0 + 7 * DAY);
    let probe = pick(&mut rig, &request(), &Quota::default());
    // The cheapest Small model that is little used.
    assert_eq!(probe.model, model("haiku"));
    assert!(probe.canary);
    assert_eq!(probe.floor, ModelTier::Medium);
    assert_eq!(rig.rolls.get(), 1);

    let busy = Quota {
        used: HashMap::from([(model("haiku"), 0.5)]),
        ..Default::default()
    };
    assert_eq!(pick(&mut rig, &request(), &busy).model, model("mini"));
}

#[test]
fn router_does_not_probe_when_it_must_not() {
    let mut rig = rig(0.0);
    raise_floor_to_medium(rig.router.ledger());
    rig.clock.set(T0 + 7 * DAY);

    let critical = RouteRequest {
        critical: true,
        ..request()
    };
    let swapped_up = RouteRequest {
        min_tier: Some(ModelTier::Medium),
        ..request()
    };
    let unknown = Quota {
        usage_unknown: true,
        ..Default::default()
    };
    let busy = Quota {
        used: HashMap::from([(model("haiku"), 0.9), (model("mini"), 0.5)]),
        ..Default::default()
    };
    for (request, quota) in [
        (critical, Quota::default()),
        (swapped_up, Quota::default()),
        (request(), unknown),
        (request(), busy),
    ] {
        let chosen = pick(&mut rig, &request, &quota);
        assert_eq!(chosen.model, model("codex"));
        assert!(!chosen.canary);
    }
    assert_eq!(rig.rolls.get(), 0);
}

#[test]
fn a_roll_of_ten_percent_or_more_is_not_a_probe() {
    let mut rig = rig(0.10);
    raise_floor_to_medium(rig.router.ledger());
    rig.clock.set(T0 + 7 * DAY);
    assert!(!pick(&mut rig, &request(), &Quota::default()).canary);
    assert_eq!(rig.rolls.get(), 1);
}

#[test]
fn seeded_probes_are_deterministic_and_near_ten_percent() {
    let run = || {
        let (_dir, ledger) = ledger();
        raise_floor_to_medium(&ledger);
        let clock = TestClock(Rc::new(Cell::new(T0 + 7 * DAY)));
        let mut router = ModelRouter::with_sources(ledger, clock, SeededRng::new(7));
        (0..500)
            .map(|_| {
                router
                    .select(&request(), &candidates(), &Quota::default())
                    .unwrap()
                    .canary
            })
            .collect::<Vec<_>>()
    };
    let first = run();
    assert_eq!(first, run());
    let probes = first.iter().filter(|probe| **probe).count();
    assert!((25..=80).contains(&probes), "{probes} probes in 500");
}

/// Floor raised to Medium, cooldown over (so a canary would be eligible), and
/// `haiku` and `flash` demoted for the class.
fn canary_ready_rig() -> Rig {
    let mut rig = rig(0.0);
    raise_floor_to_medium(rig.router.ledger());
    rig.clock.set(T0 + 7 * DAY);
    rig
}

#[test]
fn item_pin_is_used_even_below_the_floor_demoted_or_without_headroom() {
    let mut rig = rig(0.0);
    raise_floor_to_medium(rig.router.ledger()); // haiku and flash demoted, floor Medium
    let quota = Quota {
        empty: BTreeSet::from([model("haiku")]),
        ..Default::default()
    };
    let pinned = RouteRequest {
        item_pin: Some(model("haiku")),
        ..request()
    };
    let chosen = pick(&mut rig, &pinned, &quota);
    assert_eq!(chosen.model, model("haiku"));
    assert!(chosen.pinned && !chosen.canary);
    assert_eq!(rig.rolls.get(), 0);
}

#[test]
fn item_pin_is_never_escalated_by_a_swap_or_a_raised_floor() {
    let mut rig = rig(0.0);
    raise_floor_to_medium(rig.router.ledger());
    let pinned = RouteRequest {
        item_pin: Some(model("mini")),
        min_tier: Some(ModelTier::Large),
        critical: true,
        ..request()
    };
    assert_eq!(
        pick(&mut rig, &pinned, &Quota::default()).model,
        model("mini")
    );
}

#[test]
fn item_pin_is_never_canaried_when_a_canary_would_be_eligible() {
    let mut rig = canary_ready_rig();
    let pinned = RouteRequest {
        item_pin: Some(model("codex")),
        ..request()
    };
    let chosen = pick(&mut rig, &pinned, &Quota::default());
    assert_eq!(chosen.model, model("codex"));
    assert!(!chosen.canary);
    assert_eq!(rig.rolls.get(), 0);
}

#[test]
fn project_pin_is_used_and_the_item_pin_wins_over_it() {
    let mut rig = rig(0.5);
    let project = RouteRequest {
        project_pin: Some(model("opus")),
        ..request()
    };
    let chosen = pick(&mut rig, &project, &Quota::default());
    assert_eq!(chosen.model, model("opus"));
    assert!(chosen.pinned);

    let both = RouteRequest {
        item_pin: Some(model("sonnet")),
        project_pin: Some(model("opus")),
        ..request()
    };
    assert_eq!(
        pick(&mut rig, &both, &Quota::default()).model,
        model("sonnet")
    );
}

#[test]
fn project_pin_is_never_demoted_or_canaried() {
    let mut rig = canary_ready_rig();
    let project = RouteRequest {
        project_pin: Some(model("flash_unlisted_but_demoted")),
        ..request()
    };
    // Not among the candidates: an error, never a substitute.
    let err = rig
        .router
        .select(&project, &candidates(), &Quota::default())
        .unwrap_err();
    assert!(matches!(err, RouteError::PinnedModelUnavailable(_)));

    let project = RouteRequest {
        project_pin: Some(model("haiku")),
        ..request()
    };
    let chosen = pick(&mut rig, &project, &Quota::default());
    assert_eq!(chosen.model, model("haiku"));
    assert!(!chosen.canary);
    assert_eq!(rig.rolls.get(), 0);
}

#[test]
fn pinned_selection_does_not_touch_the_ledger() {
    let mut rig = rig(0.0);
    let mut stale = candidates();
    stale[3].version = "2.0".into(); // haiku shows a new version
    let pinned = RouteRequest {
        item_pin: Some(model("opus")),
        ..request()
    };
    let before = rig.router.ledger().floor(&class()).unwrap();
    rig.router
        .select(&pinned, &stale, &Quota::default())
        .unwrap();
    assert_eq!(rig.router.ledger().floor(&class()).unwrap(), before);
}

#[test]
fn experiment_pinned_item_never_triggers_canary_or_model_change() {
    let mut rig = canary_ready_rig();
    // Everything that would move an ordinary item: a raised floor, an expired
    // cooldown with a canary candidate, a demotion of the pinned model, a
    // swap-up tier and an exhausted quota.
    let quota = Quota {
        empty: BTreeSet::from([model("flash")]),
        ..Default::default()
    };
    for pin in ["codex", "haiku", "mini"] {
        let experiment = RouteRequest {
            experiment_pinned: true,
            item_pin: Some(model(pin)),
            min_tier: Some(ModelTier::Large),
            ..request()
        };
        for _ in 0..20 {
            let chosen = pick(&mut rig, &experiment, &quota);
            assert_eq!(chosen.model, model(pin));
            assert!(chosen.pinned && !chosen.canary);
        }
    }
    assert_eq!(rig.rolls.get(), 0);

    // Pinned at project level only: same.
    let project = RouteRequest {
        experiment_pinned: true,
        project_pin: Some(model("codex")),
        ..request()
    };
    let chosen = pick(&mut rig, &project, &quota);
    assert_eq!(chosen.model, model("codex"));
    assert!(!chosen.canary);

    // Without a pin there is no model to hold fixed: refuse rather than route.
    let unpinned = RouteRequest {
        experiment_pinned: true,
        ..request()
    };
    let err = rig
        .router
        .select(&unpinned, &candidates(), &quota)
        .unwrap_err();
    assert!(matches!(err, RouteError::ExperimentWithoutPin));
    assert_eq!(rig.rolls.get(), 0);
}
