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
    /// The harness can stop and ask before running a command, so a chat's
    /// Ask mode and approval rules mean something. False for a harness that
    /// executes its tools without ever asking (Cursor's SDK runs them
    /// in-process), where "would the chat ask?" is always no.
    pub approval_prompts: bool,
    /// Runs can start as a named agent (the `agent` model option).
    pub named_agents: bool,
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
        approval_prompts: true,
        named_agents: false,
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
        approval_prompts: true,
        named_agents: true,
    };
    const ACP: HarnessCapabilities = HarnessCapabilities {
        steering: SteeringMode::TurnBoundary,
        turn_end: TurnEnd::PromptedOnly,
        agent_tools: true,
        instructions: false,
        titles: false,
        account_homes: false,
        read_only: false,
        approval_prompts: true,
        named_agents: false,
    };
    match id {
        // Steers inject mid-turn (stdin / `turn/steer`); Done is the CLI's own
        // terminal frame, wake turns included.
        HarnessId::ClaudeCode | HarnessId::Codex => NATIVE,
        // No mid-turn injection, so steers queue for the next turn. Done is the
        // SDK run's result / opencode's `session.status{idle}`, every turn.
        // The per-run `opencode serve` gets the Mast MCP server through
        // `OPENCODE_CONFIG_CONTENT`.
        // opencode starts its sessions as one of its own agents.
        HarnessId::Opencode => HarnessCapabilities {
            turn_end: TurnEnd::Deterministic,
            named_agents: true,
            instructions: true,
            ..ACP
        },
        // The shim hands the Mast MCP server to the SDK's `mcpServers`, and the
        // envelope rides the first prompt of a fresh session.
        // The SDK executes tools client-side with no approval step, so it never
        // asks whatever the chat's approval mode says.
        HarnessId::Cursor => HarnessCapabilities {
            turn_end: TurnEnd::Deterministic,
            agent_tools: true,
            instructions: true,
            approval_prompts: false,
            ..ACP
        },
        // `session/prompt` owns the turn until its response, so prompted turns
        // end authoritatively; background work after it still needs the
        // watchdog. No agent advertises a `_session/steering` extension.
        // Work Outline instructions ride the first prompt of a fresh session.
        HarnessId::Devin | HarnessId::Grok | HarnessId::Hermes | HarnessId::Pi => {
            HarnessCapabilities {
                instructions: true,
                ..ACP
            }
        }
        // Instructions ride the system prompt; the CLI probe still decides.
        // A named agent's definition rides the same channel.
        HarnessId::Antigravity => HarnessCapabilities {
            instructions: true,
            named_agents: true,
            ..ACP
        },
        HarnessId::Mock => HarnessCapabilities {
            turn_end: TurnEnd::Watchdog,
            agent_tools: false,
            instructions: false,
            account_homes: false,
            named_agents: false,
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
                    "{id:?}: {:?} {:?} tools={} instructions={} titles={} homes={} read_only={} asks={} agents={}",
                    c.steering,
                    c.turn_end,
                    yes(c.agent_tools),
                    yes(c.instructions),
                    yes(c.titles),
                    yes(c.account_homes),
                    yes(c.read_only),
                    yes(c.approval_prompts),
                    yes(c.named_agents),
                )
            })
            .collect();
        assert_eq!(
            rows,
            [
                "ClaudeCode: StepBoundary Deterministic tools=yes instructions=yes titles=yes homes=yes read_only=yes asks=yes agents=yes",
                "Codex: StepBoundary Deterministic tools=yes instructions=yes titles=yes homes=yes read_only=yes asks=yes agents=yes",
                "Cursor: TurnBoundary Deterministic tools=yes instructions=yes titles=- homes=- read_only=- asks=- agents=-",
                "Devin: TurnBoundary PromptedOnly tools=yes instructions=- titles=- homes=- read_only=- asks=yes agents=-",
                "Grok: TurnBoundary PromptedOnly tools=yes instructions=- titles=- homes=- read_only=- asks=yes agents=-",
                "Hermes: TurnBoundary PromptedOnly tools=yes instructions=- titles=- homes=- read_only=- asks=yes agents=-",
                "Pi: TurnBoundary PromptedOnly tools=yes instructions=- titles=- homes=- read_only=- asks=yes agents=-",
                "Opencode: TurnBoundary Deterministic tools=yes instructions=- titles=- homes=- read_only=- asks=yes agents=yes",
                "Antigravity: TurnBoundary PromptedOnly tools=yes instructions=yes titles=- homes=- read_only=- asks=yes agents=yes",
                "Mock: StepBoundary Watchdog tools=- instructions=- titles=yes homes=- read_only=yes asks=yes agents=-",
            ]
        );
    }
}
