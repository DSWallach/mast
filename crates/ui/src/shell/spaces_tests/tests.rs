use chrono::{TimeZone as _, Utc};

use super::{compare_sidebar_chats, delete_space_copy, promote_local_device_group};

#[test]
fn delete_space_copy_names_the_index_the_outline_and_hide() {
    let one = delete_space_copy("Alpha", "Laptop", 1);
    assert!(one.contains("its 1 session on Laptop"), "{one}");
    let many = delete_space_copy("Alpha", "Laptop", 3);
    assert!(many.contains("its 3 sessions on Laptop"), "{many}");
    assert!(many.contains("leave Mast’s index"), "{many}");
    assert!(many.contains("whole Work Outline"), "{many}");
    assert!(many.contains("can’t be undone"), "{many}");
    assert!(
        many.contains("To just declutter the Outline, use Hide from Outline"),
        "{many}"
    );
}
use crate::settings::SidebarSort;

fn group(device: &str, value: u8) -> (Option<(String, String)>, Vec<u8>) {
    (Some((device.into(), device.into())), vec![value])
}

fn chat(id: &str) -> mast_proto::Chat {
    mast_proto::Chat {
        id: id.into(),
        device_id: "device".into(),
        title: None,
        archived: false,
        cwd: None,
        branch: None,
        checkout_id: None,
        source_context: None,
        config: None,
        last_message_preview: None,
        last_message_at: Some(Utc.timestamp_opt(10, 0).unwrap()),
        created_at: Utc.timestamp_opt(5, 0).unwrap(),
        harness_session_id: None,
        harness_session_cwd: None,
        actual_model: None,
        parent_chat_id: None,
        space_id: None,
        last_seen_at: None,
        room_gen: None,
        linked_pr_url: None,
        linked_pr_source: None,
        linked_ticket_id: None,
        linked_ticket_source: None,
    }
}

#[test]
fn equal_sidebar_timestamps_sort_by_stable_chat_id() {
    let alpha = chat("alpha");
    let beta = chat("beta");
    assert!(compare_sidebar_chats(SidebarSort::Created, &alpha, &beta).is_lt());
    assert!(compare_sidebar_chats(SidebarSort::LastUpdated, &alpha, &beta).is_lt());
}

#[test]
fn current_device_is_promoted_without_resorting_remote_groups() {
    let mut groups = vec![
        group("recent-remote", 1),
        group("local", 2),
        group("older-remote", 3),
    ];

    promote_local_device_group(&mut groups, Some("local"));

    let order: Vec<_> = groups
        .iter()
        .map(|(group, _)| group.as_ref().unwrap().0.as_str())
        .collect();
    assert_eq!(order, ["local", "recent-remote", "older-remote"]);
}

#[test]
fn missing_current_device_leaves_group_order_untouched() {
    let mut groups = vec![group("first", 1), group("second", 2)];
    let before = groups.clone();

    promote_local_device_group(&mut groups, Some("not-present"));

    assert_eq!(groups, before);
}
