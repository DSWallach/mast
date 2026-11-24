//! What the hosting device can observe about an external run, and the pure
//! derivation of a run's state from those observations. Everything here is
//! device-local: nothing in this module is written to the registry.

use serde::{Deserialize, Serialize};

/// Longest `why` text, in characters.
pub const MAX_WHY_CHARS: usize = 60;

/// No output for this long on a live tmux session reads as stalled.
pub const DEFAULT_STALL_AFTER_SECS: u64 = 25 * 60;

pub const DEFAULT_CAPACITY_CAP: u32 = 5;
pub const DEFAULT_STALE_DAYS: u32 = 7;

/// Space settings for the poller. The run directory, exit file and tmux
/// session are conventions of whoever starts the runs, so none has a default:
/// without them a box is listed but never probed.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct ExternalRunSettings {
    /// Boxes the account may run at once; the `gh` list does not report it.
    pub capacity_cap: u32,
    /// Account limit on codespaces that exist (Shutdown boxes count; only
    /// deleting one frees a slot). Display only; unset by default.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub box_limit: Option<u32>,
    /// A live run with no update for this many days is no longer polled.
    pub stale_days: u32,
    /// Absolute run directory on the box; `{runTag}` is replaced.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub run_dir: Option<String>,
    /// File inside the run directory holding the exit code.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub exit_file: Option<String>,
    /// File inside the run directory whose mtime is the last output.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub output_file: Option<String>,
    /// tmux session name; `{runTag}` is replaced.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tmux_session: Option<String>,
}

impl Default for ExternalRunSettings {
    fn default() -> Self {
        Self {
            capacity_cap: DEFAULT_CAPACITY_CAP,
            box_limit: None,
            stale_days: DEFAULT_STALE_DAYS,
            run_dir: None,
            exit_file: None,
            output_file: None,
            tmux_session: None,
        }
    }
}

impl ExternalRunSettings {
    pub fn probe_configured(&self) -> bool {
        self.run_dir.is_some() && self.exit_file.is_some() && self.tmux_session.is_some()
    }
}

/// `setExternalRunSettings`: replaces the Space's poller settings.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SetExternalRunSettingsParams {
    pub space_id: String,
    pub settings: ExternalRunSettings,
    pub operation_id: String,
}

/// A run's derived state.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum RunState {
    Running,
    Done,
    Failed(i32),
    Orphaned,
    Stalled,
    NeedsHuman,
    Unknown,
}

impl RunState {
    pub fn word(self) -> String {
        match self {
            Self::Running => "running".into(),
            Self::Done => "done".into(),
            Self::Failed(code) => format!("failed({code})"),
            Self::Orphaned => "orphaned".into(),
            Self::Stalled => "stalled".into(),
            Self::NeedsHuman => "needsHuman".into(),
            Self::Unknown => "unknown".into(),
        }
    }

    pub fn parse(word: &str) -> Option<Self> {
        Some(match word {
            "running" => Self::Running,
            "done" => Self::Done,
            "orphaned" => Self::Orphaned,
            "stalled" => Self::Stalled,
            "needsHuman" => Self::NeedsHuman,
            "unknown" => Self::Unknown,
            other => {
                let code = other.strip_prefix("failed(")?.strip_suffix(')')?;
                Self::Failed(code.parse().ok()?)
            }
        })
    }

    /// The run has reached an end that no later observation changes.
    pub fn is_terminal(self) -> bool {
        matches!(self, Self::Done | Self::Failed(_))
    }
}

impl Serialize for RunState {
    fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.serialize_str(&self.word())
    }
}

impl<'de> Deserialize<'de> for RunState {
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let word = String::deserialize(deserializer)?;
        Self::parse(&word).ok_or_else(|| serde::de::Error::custom("unknown run state"))
    }
}

/// Whether the host could ask GitHub at all.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", tag = "kind", content = "detail")]
pub enum GhHealth {
    Ok,
    Missing,
    NotSignedIn,
    /// A 403 that is not a rate limit: the login lacks the codespace scope.
    Forbidden,
    Unreachable(String),
}

impl GhHealth {
    fn reason(&self) -> Option<String> {
        match self {
            Self::Ok => None,
            Self::Missing => Some("gh not installed".into()),
            Self::NotSignedIn => Some("gh not signed in".into()),
            Self::Forbidden => Some("gh forbidden: check the codespace scope".into()),
            Self::Unreachable(why) => Some(format!("gh unreachable: {why}")),
        }
    }
}

/// One codespace as the list call reported it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct BoxFact {
    pub state: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_used_at: Option<i64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub idle_timeout_min: Option<u32>,
    /// Epoch seconds of the list response that carried this.
    #[serde(default)]
    pub at: i64,
}

impl BoxFact {
    fn is(&self, state: &str) -> bool {
        normalized(&self.state) == normalized(state)
    }

    pub fn is_available(&self) -> bool {
        self.is("available")
    }

    pub fn is_shutdown(&self) -> bool {
        self.is("shutdown")
    }
}

fn normalized(state: &str) -> String {
    state
        .chars()
        .filter(|c| c.is_ascii_alphanumeric())
        .map(|c| c.to_ascii_lowercase())
        .collect()
}

/// What one probe of a box saw for one run.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ProbeFact {
    /// Exit code from the exit file, when the file exists.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub exit_code: Option<i32>,
    pub tmux_up: bool,
    /// Seconds since the output file last changed.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub output_age_s: Option<u64>,
}

/// The probe, as the derivation sees it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Probe {
    /// The Space has not set the run directory conventions.
    NotConfigured,
    /// The run's name does not fit the plain characters a probe allows.
    Unsafe,
    /// The box is Available but no probe has run yet.
    Pending,
    Failed,
    Seen(ProbeFact),
}

pub struct DeriveInput<'a> {
    pub gh: &'a GhHealth,
    /// A tick of this engine has observed the run. False right after a restart.
    pub observed: bool,
    /// The run's box as the last successful list reported it; `None` when absent.
    pub box_fact: Option<&'a BoxFact>,
    pub probe: Probe,
    /// An exit code an earlier tick saw, kept across restarts.
    pub known_exit: Option<i32>,
    /// Open Required Actions on the run's item.
    pub open_actions: usize,
    pub stall_after_s: u64,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Derived {
    pub state: RunState,
    /// At most [`MAX_WHY_CHARS`] characters.
    pub why: String,
    pub exit_code: Option<i32>,
}

fn derived(state: RunState, why: impl Into<String>) -> Derived {
    Derived {
        state,
        why: clip(&why.into()),
        exit_code: None,
    }
}

fn clip(text: &str) -> String {
    if text.chars().count() <= MAX_WHY_CHARS {
        return text.to_owned();
    }
    let mut out: String = text.chars().take(MAX_WHY_CHARS - 1).collect();
    out.push('…');
    out
}

fn exited(code: i32) -> Derived {
    let state = if code == 0 {
        RunState::Done
    } else {
        RunState::Failed(code)
    };
    Derived {
        state,
        why: format!("exit {code} seen"),
        exit_code: Some(code),
    }
}

/// A run's state from what has been observed. Only a box that is exactly
/// Shutdown, or an Available box whose tmux session is gone, is orphaned: a
/// transient box state, an unreachable `gh`, or a restart with nothing observed
/// yet is "unknown", never an alarm.
pub fn derive_run_state(input: &DeriveInput<'_>) -> Derived {
    if let Some(code) = input.known_exit {
        return exited(code);
    }
    if let Probe::Seen(ProbeFact {
        exit_code: Some(code),
        ..
    }) = &input.probe
    {
        return exited(*code);
    }
    if let Some(reason) = input.gh.reason() {
        return derived(RunState::Unknown, reason);
    }
    if !input.observed {
        return derived(RunState::Unknown, "not yet observed");
    }
    let Some(box_fact) = input.box_fact else {
        return derived(RunState::Unknown, "box not in list");
    };
    if box_fact.is_shutdown() {
        return derived(RunState::Orphaned, "no exit seen; box shut down");
    }
    if !box_fact.is_available() {
        return derived(
            RunState::Unknown,
            format!("box {}", normalized_words(&box_fact.state)),
        );
    }
    let probe = match &input.probe {
        Probe::NotConfigured => return derived(RunState::Unknown, "probe not configured"),
        Probe::Unsafe => return derived(RunState::Unknown, "run name not probe-safe"),
        Probe::Pending => return derived(RunState::Unknown, "awaiting probe"),
        Probe::Failed => return derived(RunState::Unknown, "probe failed"),
        Probe::Seen(probe) => probe,
    };
    if !probe.tmux_up {
        return derived(RunState::Orphaned, "no exit seen; tmux session gone");
    }
    if input.open_actions > 0 {
        return derived(
            RunState::NeedsHuman,
            format!("{} open action(s) on the item", input.open_actions),
        );
    }
    if let Some(age) = probe.output_age_s
        && age >= input.stall_after_s
    {
        return derived(
            RunState::Stalled,
            format!("no output for {}", format_age(age)),
        );
    }
    derived(RunState::Running, "tmux up, no exit file")
}

fn normalized_words(state: &str) -> String {
    let mut out = String::new();
    for (index, c) in state.chars().enumerate() {
        if c.is_ascii_uppercase() && index > 0 {
            out.push(' ');
        }
        out.push(c.to_ascii_lowercase());
    }
    out
}

/// `40s`, `12m`, `2h10m`, `3d4h`: the two most significant units.
pub fn format_age(secs: u64) -> String {
    const MINUTE: u64 = 60;
    const HOUR: u64 = 60 * MINUTE;
    const DAY: u64 = 24 * HOUR;
    if secs < MINUTE {
        format!("{secs}s")
    } else if secs < HOUR {
        format!("{}m", secs / MINUTE)
    } else if secs < DAY {
        match (secs % HOUR) / MINUTE {
            0 => format!("{}h", secs / HOUR),
            minutes => format!("{}h{minutes}m", secs / HOUR),
        }
    } else {
        match (secs % DAY) / HOUR {
            0 => format!("{}d", secs / DAY),
            hours => format!("{}d{hours}h", secs / DAY),
        }
    }
}

/// One run's observation, as the host keeps and serves it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RunObservation {
    pub state: RunState,
    /// Epoch seconds the run entered `state`.
    pub since: i64,
    /// Epoch seconds of the last tick that observed it.
    pub seen_at: i64,
    pub why: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub exit_code: Option<i32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub idle_s: Option<u64>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Capacity {
    pub running: u32,
    pub cap: u32,
    pub total: u32,
    /// The Space's limit on existing boxes, when set.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub box_limit: Option<u32>,
    /// Epoch seconds.
    pub at: i64,
}

impl Capacity {
    /// `3/5 running, 6 boxes`, or `3/5 running, 6/15 boxes` with a box limit.
    pub fn line(&self) -> String {
        let boxes = match self.box_limit {
            Some(limit) => format!("{}/{limit} boxes", self.total),
            None => format!("{} boxes", self.total),
        };
        format!("{}/{} running, {boxes}", self.running, self.cap)
    }

    pub fn at_cap(&self) -> bool {
        self.cap > 0 && self.running >= self.cap
    }
}

/// Params of `ExternalRunsObserved`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ExternalRunsObservedParams {
    pub space_id: String,
}

/// Reply of `ExternalRunsObserved`. `hosted` is false on a device that does not
/// own the Space: it has the durable record but observed nothing.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ExternalRunsObserved {
    pub hosted: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub capacity: Option<Capacity>,
    /// Set when `gh` could not be used, with the reason.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub gh_problem: Option<String>,
    /// By run id.
    #[serde(default)]
    pub runs: std::collections::BTreeMap<String, RunObservation>,
}

#[cfg(test)]
mod tests {
    use super::*;

    fn available() -> BoxFact {
        BoxFact {
            state: "Available".into(),
            last_used_at: Some(1),
            idle_timeout_min: Some(30),
            at: 1,
        }
    }

    fn boxed(state: &str) -> BoxFact {
        BoxFact {
            state: state.into(),
            ..available()
        }
    }

    fn seen(tmux_up: bool, exit_code: Option<i32>, output_age_s: Option<u64>) -> Probe {
        Probe::Seen(ProbeFact {
            exit_code,
            tmux_up,
            output_age_s,
        })
    }

    struct Case {
        gh: GhHealth,
        observed: bool,
        box_fact: Option<BoxFact>,
        probe: Probe,
        known_exit: Option<i32>,
        open_actions: usize,
    }

    impl Default for Case {
        fn default() -> Self {
            Self {
                gh: GhHealth::Ok,
                observed: true,
                box_fact: Some(available()),
                probe: seen(true, None, Some(10)),
                known_exit: None,
                open_actions: 0,
            }
        }
    }

    fn derive(case: &Case) -> Derived {
        derive_run_state(&DeriveInput {
            gh: &case.gh,
            observed: case.observed,
            box_fact: case.box_fact.as_ref(),
            probe: case.probe.clone(),
            known_exit: case.known_exit,
            open_actions: case.open_actions,
            stall_after_s: DEFAULT_STALL_AFTER_SECS,
        })
    }

    #[test]
    fn the_derivation_table() {
        let cases: Vec<(&str, Case, RunState)> = vec![
            ("tmux up, no exit", Case::default(), RunState::Running),
            (
                "shutdown with no exit seen is orphaned",
                Case {
                    box_fact: Some(boxed("Shutdown")),
                    probe: Probe::Pending,
                    ..Case::default()
                },
                RunState::Orphaned,
            ),
            (
                "available, tmux gone, no exit is orphaned",
                Case {
                    probe: seen(false, None, None),
                    ..Case::default()
                },
                RunState::Orphaned,
            ),
            (
                "exit 0 in the probe is done",
                Case {
                    probe: seen(false, Some(0), None),
                    ..Case::default()
                },
                RunState::Done,
            ),
            (
                "exit 7 in the probe is failed(7)",
                Case {
                    probe: seen(true, Some(7), None),
                    ..Case::default()
                },
                RunState::Failed(7),
            ),
            (
                "an exit seen earlier wins on a shutdown box",
                Case {
                    box_fact: Some(boxed("Shutdown")),
                    known_exit: Some(0),
                    ..Case::default()
                },
                RunState::Done,
            ),
            (
                "an exit seen earlier survives a restart and an auth failure",
                Case {
                    gh: GhHealth::NotSignedIn,
                    observed: false,
                    box_fact: None,
                    known_exit: Some(2),
                    ..Case::default()
                },
                RunState::Failed(2),
            ),
            (
                "stale output is stalled",
                Case {
                    probe: seen(true, None, Some(DEFAULT_STALL_AFTER_SECS)),
                    ..Case::default()
                },
                RunState::Stalled,
            ),
            (
                "open actions on the item need a human",
                Case {
                    open_actions: 2,
                    ..Case::default()
                },
                RunState::NeedsHuman,
            ),
            (
                "open actions beat stalled",
                Case {
                    open_actions: 1,
                    probe: seen(true, None, Some(DEFAULT_STALL_AFTER_SECS * 4)),
                    ..Case::default()
                },
                RunState::NeedsHuman,
            ),
            (
                "open actions do not hide an exit",
                Case {
                    open_actions: 1,
                    probe: seen(true, Some(0), None),
                    ..Case::default()
                },
                RunState::Done,
            ),
            (
                "first tick after a restart is unknown",
                Case {
                    observed: false,
                    ..Case::default()
                },
                RunState::Unknown,
            ),
            (
                "a probe that failed is unknown",
                Case {
                    probe: Probe::Failed,
                    ..Case::default()
                },
                RunState::Unknown,
            ),
            (
                "an unconfigured probe is unknown",
                Case {
                    probe: Probe::NotConfigured,
                    ..Case::default()
                },
                RunState::Unknown,
            ),
            (
                "an unsafe run name is unknown",
                Case {
                    probe: Probe::Unsafe,
                    ..Case::default()
                },
                RunState::Unknown,
            ),
            (
                "a box missing from the list is unknown",
                Case {
                    box_fact: None,
                    ..Case::default()
                },
                RunState::Unknown,
            ),
        ];
        for (name, case, expected) in cases {
            assert_eq!(derive(&case).state, expected, "{name}");
        }
    }

    #[test]
    fn transient_box_states_are_never_orphaned() {
        for state in [
            "Starting",
            "ShuttingDown",
            "Shutting Down",
            "Queued",
            "Unavailable",
            "Provisioning",
            "Rebuilding",
            "Moved",
            "",
        ] {
            let derived = derive(&Case {
                box_fact: Some(boxed(state)),
                probe: Probe::Pending,
                ..Case::default()
            });
            assert_eq!(derived.state, RunState::Unknown, "{state:?}");
        }
        let derived = derive(&Case {
            box_fact: Some(boxed("ShuttingDown")),
            ..Case::default()
        });
        assert_eq!(derived.why, "box shutting down");
    }

    #[test]
    fn a_shutdown_state_matches_however_it_is_spelled() {
        for state in ["Shutdown", "shutdown", "SHUTDOWN", "Shut down"] {
            let derived = derive(&Case {
                box_fact: Some(boxed(state)),
                ..Case::default()
            });
            assert_eq!(derived.state, RunState::Orphaned, "{state}");
        }
    }

    #[test]
    fn gh_problems_are_unknown_with_the_reason_and_never_orphaned() {
        for (gh, reason) in [
            (GhHealth::Missing, "gh not installed"),
            (GhHealth::NotSignedIn, "gh not signed in"),
            (
                GhHealth::Forbidden,
                "gh forbidden: check the codespace scope",
            ),
            (
                GhHealth::Unreachable("timeout".into()),
                "gh unreachable: timeout",
            ),
        ] {
            let derived = derive(&Case {
                gh,
                box_fact: Some(boxed("Shutdown")),
                probe: Probe::Pending,
                ..Case::default()
            });
            assert_eq!(derived.state, RunState::Unknown);
            assert_eq!(derived.why, reason);
        }
    }

    #[test]
    fn why_is_clipped_to_the_limit() {
        let derived = derive(&Case {
            gh: GhHealth::Unreachable("x".repeat(200)),
            ..Case::default()
        });
        assert_eq!(derived.why.chars().count(), MAX_WHY_CHARS);
        assert!(derived.why.ends_with('…'));
    }

    #[test]
    fn exit_codes_are_carried() {
        let derived = derive(&Case {
            probe: seen(false, Some(3), None),
            ..Case::default()
        });
        assert_eq!(derived.exit_code, Some(3));
        assert_eq!(derived.why, "exit 3 seen");
    }

    #[test]
    fn state_words_round_trip() {
        for state in [
            RunState::Running,
            RunState::Done,
            RunState::Failed(-1),
            RunState::Failed(137),
            RunState::Orphaned,
            RunState::Stalled,
            RunState::NeedsHuman,
            RunState::Unknown,
        ] {
            assert_eq!(RunState::parse(&state.word()), Some(state));
            let json = serde_json::to_string(&state).unwrap();
            assert_eq!(serde_json::from_str::<RunState>(&json).unwrap(), state);
        }
        assert_eq!(RunState::parse("failed(x)"), None);
        assert_eq!(RunState::parse("bogus"), None);
    }

    #[test]
    fn ages_use_the_two_most_significant_units() {
        for (secs, text) in [
            (0, "0s"),
            (40, "40s"),
            (60, "1m"),
            (3_599, "59m"),
            (3_600, "1h"),
            (7_800, "2h10m"),
            (86_400, "1d"),
            (86_400 + 4 * 3_600 + 59, "1d4h"),
        ] {
            assert_eq!(format_age(secs), text);
        }
    }

    #[test]
    fn capacity_reads_as_one_line_and_detects_the_cap() {
        let capacity = Capacity {
            running: 3,
            cap: 5,
            total: 6,
            box_limit: None,
            at: 0,
        };
        assert_eq!(capacity.line(), "3/5 running, 6 boxes");
        let limited = Capacity {
            box_limit: Some(15),
            ..capacity
        };
        assert_eq!(limited.line(), "3/5 running, 6/15 boxes");
        assert!(
            !Capacity {
                total: 15,
                ..limited
            }
            .at_cap()
        );
        assert!(!capacity.at_cap());
        assert!(
            Capacity {
                running: 5,
                ..capacity
            }
            .at_cap()
        );
        assert!(
            !Capacity {
                running: 9,
                cap: 0,
                ..capacity
            }
            .at_cap()
        );
    }

    #[test]
    fn settings_default_to_a_cap_of_five_and_no_probe() {
        let settings: ExternalRunSettings = serde_json::from_str("{}").unwrap();
        assert_eq!(settings.capacity_cap, 5);
        assert!(!settings.probe_configured());
    }
}
