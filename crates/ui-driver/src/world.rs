//! Deterministic description of the small fixture world.
//!
//! This describes the fixture only. It does not materialize engine stores or
//! repositories and is not sufficient to launch the driver.

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SpaceFixture {
    pub id: String,
    pub repository: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WorktreeFixture {
    pub space_id: String,
    pub path: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ChatFixture {
    pub id: String,
    pub space_id: String,
    pub worktree: String,
    pub transcript: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ItemFixture {
    pub id: String,
    pub space_id: String,
    pub title: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RequestFixture {
    pub item_id: String,
    pub chat_id: String,
    pub text: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SmallWorld {
    pub seed: u64,
    pub clock_ms: i64,
    pub spaces: Vec<SpaceFixture>,
    pub worktrees: Vec<WorktreeFixture>,
    pub chats: Vec<ChatFixture>,
    pub items: Vec<ItemFixture>,
    pub requests: Vec<RequestFixture>,
}

impl SmallWorld {
    /// Construct fixture identities and content without consulting the host
    /// clock, random source, account, or filesystem.
    pub fn new(seed: u64, clock_ms: i64) -> Self {
        let mut state = seed;
        let mut id = |kind: &str| {
            // SplitMix64 makes the sequence independent of platform hashers.
            state = state.wrapping_add(0x9e3779b97f4a7c15);
            let mut value = state;
            value = (value ^ (value >> 30)).wrapping_mul(0xbf58476d1ce4e5b9);
            value = (value ^ (value >> 27)).wrapping_mul(0x94d049bb133111eb);
            format!("{kind}-{:016x}", value ^ (value >> 31))
        };
        let spaces: Vec<_> = (0..1)
            .map(|index| SpaceFixture {
                id: id("space"),
                repository: format!("space-{index}"),
            })
            .collect();
        let worktrees: Vec<_> = (0..3)
            .map(|index| WorktreeFixture {
                space_id: spaces[index % spaces.len()].id.clone(),
                path: format!("worktree-{index}"),
            })
            .collect();
        let chats: Vec<_> = (0..5)
            .map(|index| {
                let worktree = &worktrees[index % worktrees.len()];
                ChatFixture {
                    id: id("chat"),
                    space_id: worktree.space_id.clone(),
                    worktree: worktree.path.clone(),
                    transcript: format!("Fixture conversation {index}"),
                }
            })
            .collect();
        let items: Vec<_> = (0..12)
            .map(|index| ItemFixture {
                id: id("item"),
                space_id: spaces[index % spaces.len()].id.clone(),
                title: format!("Fixture item {index}"),
            })
            .collect();
        let requests = (0..2)
            .map(|index| RequestFixture {
                item_id: items[index].id.clone(),
                chat_id: chats[index].id.clone(),
                text: format!("Review fixture item {index}"),
            })
            .collect();
        Self {
            seed,
            clock_ms,
            spaces,
            worktrees,
            chats,
            items,
            requests,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn seed_and_clock_determine_the_world_description() {
        let first = SmallWorld::new(42, 946_684_800_000);
        assert_eq!(first, SmallWorld::new(42, 946_684_800_000));
        assert_ne!(first, SmallWorld::new(43, 946_684_800_000));
        assert_eq!(
            (
                first.spaces.len(),
                first.worktrees.len(),
                first.chats.len(),
                first.items.len(),
                first.requests.len()
            ),
            (1, 3, 5, 12, 2)
        );
        for request in &first.requests {
            let item = first
                .items
                .iter()
                .find(|item| item.id == request.item_id)
                .unwrap();
            let chat = first
                .chats
                .iter()
                .find(|chat| chat.id == request.chat_id)
                .unwrap();
            assert_eq!(item.space_id, chat.space_id);
        }
    }
}
