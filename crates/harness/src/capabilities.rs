//! What each harness can do, in one table keyed by [`HarnessId`].
//!
//! The engine and UI ask this table instead of special-casing harness ids.
//! Mast's drivers return their row from
//! [`Harness::capabilities`](crate::Harness::capabilities), and the trait's
//! capability methods read it, so a driver gains a feature by implementing it
//! and flipping its row here.

use mast_proto::{HarnessId, SteeringMode};

/// How the end of a turn is known.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TurnEnd {
    /// Every turn, prompted or agent-initiated, ends with a `Done` from the
    /// agent's own wire. The engine retires its quiesce watchdogs.
    Deterministic,
    /// Prompted turns end authoritatively; agent-initiated activity still
    /// needs the watchdog backstop.
    PromptedOnly,
    /// No authoritative signal; the watchdog decides.
    Watchdog,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct HarnessCapabilities {
    pub steering: SteeringMode,
    pub turn_end: TurnEnd,
    /// The Mast MCP server can be injected into runs.
    pub agent_tools: bool,
    /// The driver has a channel for Work Outline instructions. Whether it
    /// works on this device is still up to the CLI probe.
    pub instructions: bool,
    /// The driver has a restricted title-generation path.
    pub titles: bool,
    /// Runs can be pointed at a per-account CLI home.
    pub account_homes: bool,
    /// A read-only sandbox can be enforced.
    pub read_only: bool,
}

impl HarnessCapabilities {
    /// What an implementation that claims no row gets: test fakes standing
    /// in for a real harness keep conservative behavior.
    pub const NONE: Self = Self {
        steering: SteeringMode::TurnBoundary,
        turn_end: TurnEnd::Watchdog,
        agent_tools: false,
        instructions: false,
        titles: false,
        account_homes: false,
        read_only: false,
    };
}

pub fn capabilities(id: HarnessId) -> HarnessCapabilities {
    const NATIVE: HarnessCapabilities = HarnessCapabilities {
        steering: SteeringMode::StepBoundary,
        turn_end: TurnEnd::Deterministic,
        agent_tools: true,
        instructions: true,
        titles: true,
        account_homes: true,
        read_only: true,
    };
    const ACP: HarnessCapabilities = HarnessCapabilities {
        steering: SteeringMode::TurnBoundary,
        turn_end: TurnEnd::PromptedOnly,
        agent_tools: true,
        instructions: false,
        titles: false,
        account_homes: false,
        read_only: false,
    };
    match id {
        // Steers inject mid-turn (stdin / `turn/steer`); Done is the CLI's own
        // terminal frame, wake turns included.
        HarnessId::ClaudeCode | HarnessId::Codex => NATIVE,
        // No mid-turn injection, so steers queue for the next turn. Done is the
        // SDK run's result / opencode's `session.status{idle}`, every turn.
        // The per-run `opencode serve` gets the Mast MCP server through
        // `OPENCODE_CONFIG_CONTENT`.
        HarnessId::Opencode => HarnessCapabilities {
            turn_end: TurnEnd::Deterministic,
            ..ACP
        },
        // The shim hands the Mast MCP server to the SDK's `mcpServers`, and the
        // envelope rides the first prompt of a fresh session.
        HarnessId::Cursor => HarnessCapabilities {
            turn_end: TurnEnd::Deterministic,
            agent_tools: true,
            instructions: true,
            ..ACP
        },
        // `session/prompt` owns the turn until its response, so prompted turns
        // end authoritatively; background work after it still needs the
        // watchdog. No agent advertises a `_session/steering` extension.
        HarnessId::Devin | HarnessId::Grok | HarnessId::Hermes | HarnessId::Pi => ACP,
        // Instructions ride the system prompt; the CLI probe still decides.
        HarnessId::Antigravity => HarnessCapabilities {
            instructions: true,
            ..ACP
        },
        HarnessId::Mock => HarnessCapabilities {
            turn_end: TurnEnd::Watchdog,
            agent_tools: false,
            instructions: false,
            account_homes: false,
            ..NATIVE
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The parity table. A change here is a deliberate capability change.
    #[test]
    fn capability_matrix() {
        let yes = |b: bool| if b { "yes" } else { "-" };
        let rows: Vec<String> = HarnessId::ALL
            .iter()
            .map(|&id| {
                let c = capabilities(id);
                format!(
                    "{id:?}: {:?} {:?} tools={} instructions={} titles={} homes={} read_only={}",
                    c.steering,
                    c.turn_end,
                    yes(c.agent_tools),
                    yes(c.instructions),
                    yes(c.titles),
                    yes(c.account_homes),
                    yes(c.read_only),
                )
            })
            .collect();
        assert_eq!(
            rows,
            [
                "ClaudeCode: StepBoundary Deterministic tools=yes instructions=yes titles=yes homes=yes read_only=yes",
                "Codex: StepBoundary Deterministic tools=yes instructions=yes titles=yes homes=yes read_only=yes",
                "Cursor: TurnBoundary Deterministic tools=yes instructions=yes titles=- homes=- read_only=-",
                "Devin: TurnBoundary PromptedOnly tools=yes instructions=- titles=- homes=- read_only=-",
                "Grok: TurnBoundary PromptedOnly tools=yes instructions=- titles=- homes=- read_only=-",
                "Hermes: TurnBoundary PromptedOnly tools=yes instructions=- titles=- homes=- read_only=-",
                "Pi: TurnBoundary PromptedOnly tools=yes instructions=- titles=- homes=- read_only=-",
                "Opencode: TurnBoundary Deterministic tools=yes instructions=- titles=- homes=- read_only=-",
                "Antigravity: TurnBoundary PromptedOnly tools=yes instructions=yes titles=- homes=- read_only=-",
                "Mock: StepBoundary Watchdog tools=- instructions=- titles=yes homes=- read_only=yes",
            ]
        );
    }
}
