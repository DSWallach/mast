//! Work Outline instruction delivery for Cursor.
//!
//! @cursor/sdk 1.0.31 has an `AgentOptions.systemPrompt`, but it replaces
//! Cursor's whole built-in prompt, is gated server-side (callers without
//! access get `InvalidArgument` on the first send) and is not kept on
//! `Agent.resume`. It has no rules or append option. So the rendered envelope
//! rides the FIRST prompt of a fresh session, ahead of the user's text, and
//! the session's own history carries it from then on. A resumed session whose
//! applied revision is unchanged (`Continue`) gets nothing added.
//!
//! A revision change inside an existing session is not offered: the old
//! envelope would stay in the conversation next to the new one. The replace
//! strategy is `FreshSession`, so the engine starts a new session after the
//! user confirms the context reset, and this adapter refuses a `Replace`
//! aimed at an existing session.
//!
//! Summaries: Cursor can summarize a long conversation, which may reduce the
//! envelope to a paraphrase. The shim sees that (`summary-completed`), marks
//! the owned store, and restates the envelope ahead of the next prompt, in
//! this process or in a later one that resumes the conversation, then clears
//! the mark. [`repin_prefix`] is the text it adds.

use mast_proto::{HarnessId, InstructionTransition, RunRequest};

use crate::{HarnessError, InstructionChannel, InstructionReplaceStrategy, InstructionSupport};

/// The rendered envelope stays far below the registry budget; this bounds the
/// one prompt that carries it.
pub const MAX_INSTRUCTION_BYTES: usize = 64 * 1024;

pub fn support() -> InstructionSupport {
    InstructionSupport::Supported {
        channel: InstructionChannel::FirstTurnPrompt,
        replace_strategy: InstructionReplaceStrategy::FreshSession,
        max_bytes: MAX_INSTRUCTION_BYTES,
    }
}

/// Refuse a revision change aimed at an existing session. The engine starts a
/// new session instead, after the user confirmed the context reset.
pub fn check_transition(request: &RunRequest) -> Result<(), HarnessError> {
    match (&request.instructions, &request.resume) {
        (Some(delivery), Some(_))
            if matches!(delivery.transition, InstructionTransition::Replace { .. }) =>
        {
            Err(HarnessError::Capability {
                harness: HarnessId::Cursor,
                detail: "Cursor cannot replace Work Outline instructions inside an existing \
                         session; the new revision needs a fresh Cursor session"
                    .into(),
                upgrade_hint: None,
            })
        }
        _ => Ok(()),
    }
}

/// Whether this run's first prompt carries the envelope (the session does not
/// have the revision yet).
pub fn in_first_prompt(request: &RunRequest) -> bool {
    request
        .instructions
        .as_ref()
        .is_some_and(|delivery| !matches!(delivery.transition, InstructionTransition::Continue))
}

/// The first prompt of this run: the envelope ahead of the user's text when
/// the session does not carry the revision yet, the plain prompt otherwise.
pub fn first_prompt(request: &RunRequest) -> String {
    match &request.instructions {
        Some(delivery) if in_first_prompt(request) => {
            format!(
                "{}\n\n---\nThe instructions above apply to this whole session. The task \
                 follows.\n\n{}",
                delivery.envelope.rendered, request.prompt
            )
        }
        _ => request.prompt.clone(),
    }
}

/// What the shim puts ahead of the next prompt once Cursor has summarized the
/// conversation: the envelope again, said to be a restatement. `None` for a
/// run without instructions.
pub fn repin_prefix(request: &RunRequest) -> Option<String> {
    request.instructions.as_ref().map(|delivery| {
        format!(
            "{}\n\n---\nThe instructions above are restated because this conversation was \
             summarized. They still apply to this whole session and replace any earlier copy \
             or paraphrase of them. The task follows.\n\n",
            delivery.envelope.rendered
        )
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use mast_proto::{InstructionDelivery, InstructionEnvelope};

    fn request(transition: InstructionTransition, resume: Option<&str>) -> RunRequest {
        let mut request: RunRequest = serde_json::from_value(serde_json::json!({
            "prompt": "do the task", "model": "auto", "reasoning": null, "cwd": "/w",
            "sandbox": "workspace-write", "resume": resume
        }))
        .unwrap();
        request.instructions = Some(InstructionDelivery {
            envelope: InstructionEnvelope {
                revision_id: "rev-1".into(),
                revision_hash: "hash-1".into(),
                envelope_version: 1,
                envelope_sha256: mast_proto::envelope_sha256("RULES: report to your parent"),
                rendered: "RULES: report to your parent".into(),
            },
            transition,
        });
        request
    }

    #[test]
    fn fresh_sessions_carry_the_envelope_ahead_of_the_task() {
        let prompt = first_prompt(&request(InstructionTransition::Fresh, None));
        let (envelope, task) = (
            prompt.find("RULES: report").unwrap(),
            prompt.find("do the task").unwrap(),
        );
        assert!(envelope < task, "{prompt}");
    }

    #[test]
    fn continued_sessions_do_not_repeat_the_envelope() {
        let prompt = first_prompt(&request(InstructionTransition::Continue, Some("agent-1")));
        assert_eq!(prompt, "do the task");
    }

    #[test]
    fn the_restatement_carries_the_envelope_and_says_it_is_one() {
        for transition in [InstructionTransition::Fresh, InstructionTransition::Continue] {
            let request = request(transition, Some("agent-1"));
            let text = repin_prefix(&request).expect("a run with instructions can restate them");
            assert!(text.starts_with("RULES: report to your parent"), "{text}");
            assert!(text.contains("restated"), "{text}");
            assert!(text.ends_with("The task follows.\n\n"), "{text}");
        }
        let mut none = request(InstructionTransition::Fresh, None);
        none.instructions = None;
        assert_eq!(repin_prefix(&none), None);
    }

    #[test]
    fn only_a_session_without_the_revision_has_it_in_the_first_prompt() {
        assert!(in_first_prompt(&request(InstructionTransition::Fresh, None)));
        assert!(!in_first_prompt(&request(InstructionTransition::Continue, Some("a"))));
    }

    #[test]
    fn a_replace_aimed_at_an_existing_session_is_refused() {
        let replace = InstructionTransition::Replace {
            previous_revision_id: "r1".into(),
        };
        assert!(check_transition(&request(replace.clone(), Some("agent-1"))).is_err());
        assert!(check_transition(&request(replace, None)).is_ok());
        assert!(check_transition(&request(InstructionTransition::Continue, Some("a"))).is_ok());
    }
}
