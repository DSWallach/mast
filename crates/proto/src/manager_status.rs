//! The Manager status record: one artifact on a project's item that holds
//! what a replacement manager needs to carry on. Every update is a new
//! revision of the same artifact, with the same fixed sections.

/// Role of the artifact. The briefing lookup finds it by this, never by title.
pub const MANAGER_STATUS_ROLE: &str = "manager-status";
pub const MANAGER_STATUS_TITLE: &str = "Manager status";
/// `briefing_read` kind that selects it.
pub const MANAGER_STATUS_KIND: &str = "manager-status";
/// Largest document the inline publish accepts.
pub const MANAGER_STATUS_MAX_BYTES: usize = 64 * 1024;
/// How many of the user's latest requests the record keeps, verbatim.
pub const MANAGER_STATUS_RECENT_REQUESTS: usize = 5;

/// The sections, in order. Each is a level-two heading.
pub const MANAGER_STATUS_SECTIONS: [&str; 6] = [
    "Goals and priorities",
    "Active work items and workers",
    "Decisions",
    "Open questions",
    "Standing preferences",
    "Recent user requests",
];

/// Whether `role` names the status record, in any case.
pub fn is_manager_status_role(role: &str) -> bool {
    role.trim().eq_ignore_ascii_case(MANAGER_STATUS_ROLE)
}

/// Check a status document: size, and every section heading present once, in
/// order. Section bodies are free text; an empty section says "None".
pub fn validate_manager_status(content: &str) -> Result<(), String> {
    if content.trim().is_empty() {
        return Err("the status document is empty".into());
    }
    if content.len() > MANAGER_STATUS_MAX_BYTES {
        return Err(format!(
            "the status document is over {} KB; keep each section short",
            MANAGER_STATUS_MAX_BYTES / 1024
        ));
    }
    let stamped = content
        .lines()
        .map(str::trim)
        .find(|line| !line.is_empty())
        .and_then(|line| line.strip_prefix("Updated:"))
        .is_some_and(|rest| !rest.trim().is_empty());
    if !stamped {
        return Err(
            "start the document with a line `Updated: <date and time>`; an unchanged document records no revision"
                .into(),
        );
    }
    let headings: Vec<&str> = content
        .lines()
        .filter_map(|line| line.strip_prefix("## "))
        .map(str::trim)
        .collect();
    let expected = MANAGER_STATUS_SECTIONS;
    if headings != expected {
        return Err(format!(
            "the status document needs exactly these level-two headings, in order: {}. Found: {}",
            expected.join(" | "),
            if headings.is_empty() {
                "none".to_owned()
            } else {
                headings.join(" | ")
            }
        ));
    }
    Ok(())
}

/// Recorded on a revision when the inline publish writes it: which role
/// published it and which chat held that role at the time. It lives with the
/// publication, so it holds after a hand-off deletes the chat. Absent for a
/// revision anyone else wrote and for revisions made before it existed.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ManagerStatusStamp {
    /// The role row id, `manager:<project item id>`.
    pub role_id: String,
    pub chat_id: String,
}

/// The manager's standing rule for keeping the record current. One bullet:
/// the built-in instructions prefix it with "- ".
pub const MANAGER_STATUS_RULE: &str = "Keep the Manager status current. At the end of every turn that changed \
state (you created or dispatched a worker or item, changed a status, answered or raised a Required Action, or \
the user stated a goal, decision or preference), call `artifact_publish` once with `role: \"manager-status\"`, \
`title: \"Manager status\"` and the whole document as inline `content` (no `path`; you cannot write files). \
Start with a line `Updated: <current date and time>`, then use exactly these level-two sections in order: Goals and priorities; Active work items and workers; Decisions; \
Open questions; Standing preferences; Recent user requests (the last 5, quoted verbatim). Write \"None\" for an \
empty section. Rewrite the whole document each time so it stands alone for a successor.";

/// What Mast sends when a state-changing turn ended without a new revision.
/// It stands alone: a manager whose instructions predate the status rule can
/// still do exactly what it asks.
pub fn manager_status_nudge_text(baseline_revision: u32) -> String {
    let basis = if baseline_revision == 0 {
        "no revision exists yet".to_owned()
    } else {
        format!("the latest revision is {baseline_revision}")
    };
    format!(
        "{} for the Manager status record]\nYour last turn changed state but did not publish a new Manager status ({basis}). \
Publish it now with `artifact_publish`: `role: \"manager-status\"`, `title: \"Manager status\"`, the whole document as inline `content` (no `path`), a one-line `note` and an `idempotencyKey`. \
Start the document with a line `Updated: <current date and time>`, then exactly these level-two sections in order: {sections}. \
Put the last {recent} user requests word for word under the last one, and write \"None\" under an empty section. Then stop; do not do other work in this turn.",
        crate::SYSTEM_MESSAGE_PREFIX,
        sections = MANAGER_STATUS_SECTIONS.join("; "),
        recent = MANAGER_STATUS_RECENT_REQUESTS,
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    fn document() -> String {
        let sections = MANAGER_STATUS_SECTIONS
            .iter()
            .map(|s| format!("## {s}\nNone\n"))
            .collect::<Vec<_>>()
            .join("\n");
        format!("Updated: 2026-11-18 09:00\n\n{sections}")
    }

    #[test]
    fn a_complete_document_validates() {
        assert_eq!(validate_manager_status(&document()), Ok(()));
    }

    #[test]
    fn missing_reordered_or_extra_sections_are_refused() {
        let missing = document().replace("## Decisions\nNone\n", "");
        assert!(validate_manager_status(&missing).unwrap_err().contains("Found:"));
        let extra = format!("{}\n## Notes\nx\n", document());
        assert!(validate_manager_status(&extra).is_err());
        let mut swapped = MANAGER_STATUS_SECTIONS;
        swapped.swap(0, 1);
        let text: String = std::iter::once("Updated: now\n".to_owned())
            .chain(swapped.iter().map(|s| format!("## {s}\nx\n")))
            .collect();
        assert!(validate_manager_status(&text).is_err());
        assert!(validate_manager_status("  \n").is_err());
    }

    #[test]
    fn the_document_must_carry_an_updated_line_first() {
        let plain = document().replacen("Updated: 2026-11-18 09:00\n\n", "", 1);
        assert!(validate_manager_status(&plain).unwrap_err().contains("Updated:"));
        let blank = document().replacen("Updated: 2026-11-18 09:00", "Updated:", 1);
        assert!(validate_manager_status(&blank).is_err());
    }

    #[test]
    fn an_oversized_document_is_refused() {
        let big = format!("{}{}", document(), "x".repeat(MANAGER_STATUS_MAX_BYTES));
        assert!(validate_manager_status(&big).unwrap_err().contains("KB"));
    }

    #[test]
    fn the_role_matches_in_any_case_and_the_rule_names_every_section() {
        assert!(is_manager_status_role(" Manager-Status "));
        assert!(!is_manager_status_role("plan"));
        for section in MANAGER_STATUS_SECTIONS {
            assert!(MANAGER_STATUS_RULE.contains(section), "{section}");
        }
        assert!(!MANAGER_STATUS_RULE.starts_with('-'));
    }

    #[test]
    fn the_stamp_round_trips_and_is_optional_on_a_revision() {
        let stamp = ManagerStatusStamp { role_id: "manager:p".into(), chat_id: "c".into() };
        let value = serde_json::to_value(&stamp).unwrap();
        assert_eq!(value, serde_json::json!({"roleId": "manager:p", "chatId": "c"}));
        let revision: crate::artifacts::ArtifactRevision =
            serde_json::from_value(serde_json::json!({"id": 1})).unwrap();
        assert!(revision.manager_status.is_none());
        let mut with = revision.clone();
        with.manager_status = Some(stamp);
        let back: crate::artifacts::ArtifactRevision =
            serde_json::from_value(serde_json::to_value(&with).unwrap()).unwrap();
        assert_eq!(back.manager_status, with.manager_status);
        assert!(serde_json::to_value(&revision).unwrap().get("managerStatus").is_none());
    }

    #[test]
    fn the_nudge_is_a_system_message_naming_the_revision() {
        assert!(manager_status_nudge_text(3).starts_with(crate::SYSTEM_MESSAGE_PREFIX));
        assert!(manager_status_nudge_text(3).contains("revision is 3"));
        assert!(manager_status_nudge_text(0).contains("no revision exists"));
        // It stands alone: the sections, the first line and the inline content.
        let text = manager_status_nudge_text(1);
        for section in MANAGER_STATUS_SECTIONS {
            assert!(text.contains(section), "{section}");
        }
        assert!(text.contains("Updated:") && text.contains("inline `content`"));
    }
}
