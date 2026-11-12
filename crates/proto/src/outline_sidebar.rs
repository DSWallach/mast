use std::collections::{HashMap, HashSet};

use crate::{SidebarSection, WorkOutlineSnapshot};

pub const OUTLINE_SIDEBAR_SECTION_PREFIX: &str = "outline:";

/// Project the outline into sidebar groups without duplicating its membership.
pub fn outline_sidebar_sections(snapshot: &WorkOutlineSnapshot) -> Vec<SidebarSection> {
    if !snapshot.available {
        return Vec::new();
    }
    let items: HashMap<_, _> = snapshot
        .items
        .iter()
        .map(|item| (item.id.as_str(), item))
        .collect();
    let mut sections = Vec::new();
    let mut destinations = HashMap::new();
    for item in &snapshot.items {
        let mut ancestors = Vec::new();
        let mut current = Some(item.id.as_str());
        let mut seen = HashSet::new();
        while let Some(id) = current {
            if !seen.insert(id) {
                ancestors.clear();
                break;
            }
            let Some(ancestor) = items.get(id) else {
                ancestors.clear();
                break;
            };
            if ancestor.archived_at.is_some() {
                ancestors.clear();
                break;
            }
            ancestors.push(*ancestor);
            current = ancestor.parent_work_item_id.as_deref();
        }
        let Some(root) = ancestors.last() else {
            continue;
        };
        let destination = ancestors
            .iter()
            .find(|ancestor| ancestor.is_project)
            .copied()
            .unwrap_or(root);
        destinations.insert(item.id.as_str(), destination.id.as_str());
        if destination.id != item.id {
            continue;
        }
        if item.parent_work_item_id.is_none()
            && item.chat_ids.is_empty()
            && !item.child_ids.is_empty()
            && item
                .child_ids
                .iter()
                .all(|id| items.get(id.as_str()).is_some_and(|child| child.is_project))
        {
            continue;
        }
        sections.push(SidebarSection {
            id: format!(
                "{OUTLINE_SIDEBAR_SECTION_PREFIX}{}:{}",
                snapshot.space_id, item.id
            ),
            name: ancestors
                .iter()
                .rev()
                .map(|ancestor| ancestor.title.as_str())
                .collect::<Vec<_>>()
                .join(" / "),
            session_ids: Vec::new(),
            collapsed: false,
        });
    }
    let indices: HashMap<_, _> = sections
        .iter()
        .enumerate()
        .map(|(index, section)| (section.id.clone(), index))
        .collect();
    for item in &snapshot.items {
        let Some(destination) = destinations.get(item.id.as_str()) else {
            continue;
        };
        let id = format!(
            "{OUTLINE_SIDEBAR_SECTION_PREFIX}{}:{destination}",
            snapshot.space_id
        );
        if let Some(index) = indices.get(&id) {
            sections[*index]
                .session_ids
                .extend(item.chat_ids.iter().cloned());
        }
    }
    for section in &mut sections {
        section.session_ids.sort();
        section.session_ids.dedup();
    }
    sections
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn snapshot() -> WorkOutlineSnapshot {
        let mut snapshot = WorkOutlineSnapshot::unavailable("space", true, 1);
        snapshot.available = true;
        snapshot.items = serde_json::from_value(json!([
            {"id":"projects","spaceId":"space","title":"Projects","childIds":["compliance"]},
            {"id":"compliance","spaceId":"space","title":"Compliance","parentWorkItemId":"projects","isProject":true,"childIds":["ticket"]},
            {"id":"ticket","spaceId":"space","title":"Ticket","parentWorkItemId":"compliance","chatIds":["chat"]},
            {"id":"archive","spaceId":"space","title":"Archive","childIds":["old"]},
            {"id":"old","spaceId":"space","title":"Old reviews","parentWorkItemId":"archive","chatIds":["old-chat"]}
        ])).unwrap();
        snapshot
    }

    #[test]
    fn outline_sidebar_sections_follow_projects_and_root_groups() {
        let sections = outline_sidebar_sections(&snapshot());
        assert_eq!(sections.len(), 2);
        assert_eq!(sections[0].name, "Projects / Compliance");
        assert_eq!(sections[0].session_ids, ["chat"]);
        assert_eq!(sections[1].name, "Archive");
        assert_eq!(sections[1].session_ids, ["old-chat"]);
    }

    #[test]
    fn outline_sidebar_sections_follow_rename_reparent_and_disassociation() {
        let mut snapshot = snapshot();
        let original_id = outline_sidebar_sections(&snapshot)[0].id.clone();
        snapshot.items[1].title = "New name".into();
        assert_eq!(outline_sidebar_sections(&snapshot)[0].id, original_id);
        assert_eq!(
            outline_sidebar_sections(&snapshot)[0].name,
            "Projects / New name"
        );
        snapshot.items[2].parent_work_item_id = Some("archive".into());
        let sections = outline_sidebar_sections(&snapshot);
        assert!(sections[0].session_ids.is_empty());
        assert_eq!(sections[1].session_ids, ["chat", "old-chat"]);
        snapshot.items[2].chat_ids.clear();
        assert_eq!(
            outline_sidebar_sections(&snapshot)[1].session_ids,
            ["old-chat"]
        );
    }

    #[test]
    fn outline_sidebar_sections_ignore_archived_or_unavailable_trees() {
        let mut snapshot = snapshot();
        snapshot.items[1].archived_at = Some(1);
        assert_eq!(outline_sidebar_sections(&snapshot).len(), 1);
        snapshot.available = false;
        assert!(outline_sidebar_sections(&snapshot).is_empty());
    }
}
