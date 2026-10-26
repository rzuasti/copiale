//! The new group picker: members chosen from contacts, then a name.

use copiale_core::text::group_title;
use copiale_core::{Command, MAX_GROUP_MEMBERS, ServiceId};

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum GroupStep {
    #[default]
    Members,
    Name,
}

#[derive(Debug, Default)]
pub struct NewGroup {
    step: GroupStep,
    /// In the order picked.
    members: Vec<ServiceId>,
    name: String,
    /// Waiting for the core.
    creating: bool,
    /// Why the last attempt failed.
    error: Option<String>,
}

impl NewGroup {
    pub fn step(&self) -> GroupStep {
        self.step
    }

    pub fn members(&self) -> &[ServiceId] {
        &self.members
    }

    pub fn name(&self) -> &str {
        &self.name
    }

    pub fn creating(&self) -> bool {
        self.creating
    }

    pub fn error(&self) -> Option<&str> {
        self.error.as_deref()
    }

    pub fn is_member(&self, id: ServiceId) -> bool {
        self.members.contains(&id)
    }

    /// Adds `id`, or removes them if picked.
    pub(super) fn toggle(&mut self, id: ServiceId) {
        if self.is_member(id) {
            self.remove(id);
        } else if self.members.len() < MAX_GROUP_MEMBERS {
            self.members.push(id);
        }
    }

    pub(super) fn remove(&mut self, id: ServiceId) {
        self.members.retain(|member| *member != id);
    }

    /// On to naming the group, once someone is picked.
    pub(super) fn next(&mut self) -> bool {
        let ready = self.step == GroupStep::Members && !self.members.is_empty();
        if ready {
            self.step = GroupStep::Name;
        }
        ready
    }

    /// Back to picking members; `false` if already there.
    pub(super) fn back(&mut self) -> bool {
        let named = self.step == GroupStep::Name && !self.creating;
        if named {
            self.step = GroupStep::Members;
        }
        named
    }

    pub(super) fn set_name(&mut self, name: &str) {
        self.name = name.into();
    }

    /// Whether the name is one a group can have.
    pub fn name_valid(&self) -> bool {
        group_title(&self.name).is_some()
    }

    /// Asks the core to create the group, unless it is already.
    pub(super) fn create(&mut self) -> Option<Command> {
        if self.step != GroupStep::Name || self.creating || !self.name_valid() {
            return None;
        }
        self.creating = true;
        self.error = None;
        Some(Command::CreateGroup {
            title: self.name.clone(),
            members: self.members.clone(),
        })
    }

    /// Whether a creation was running; it isn't now.
    pub(super) fn finished(&mut self, error: Option<String>) -> bool {
        let creating = std::mem::take(&mut self.creating);
        if creating {
            self.error = error;
        }
        creating
    }
}

#[cfg(test)]
mod tests {
    use copiale_core::text::MAX_GROUP_TITLE;
    use uuid::Uuid;

    use super::*;

    fn id(n: u128) -> ServiceId {
        ServiceId::new(Uuid::from_u128(n))
    }

    #[test]
    fn members_toggle_in_the_order_picked() {
        let mut group = NewGroup::default();
        group.toggle(id(2));
        group.toggle(id(1));
        group.toggle(id(3));
        group.toggle(id(2));
        assert_eq!(group.members(), [id(1), id(3)]);
        group.remove(id(1));
        assert_eq!(group.members(), [id(3)]);
    }

    #[test]
    fn naming_needs_a_member_and_going_back_keeps_them() {
        let mut group = NewGroup::default();
        assert!(!group.next());
        assert!(!group.back());
        group.toggle(id(1));
        assert!(group.next());
        assert_eq!(group.step(), GroupStep::Name);
        assert!(group.back());
        assert_eq!(group.step(), GroupStep::Members);
        assert_eq!(group.members(), [id(1)]);
    }

    #[test]
    fn creating_needs_a_valid_name_and_runs_once() {
        let mut group = NewGroup::default();
        group.toggle(id(1));
        group.set_name("Trip");
        assert!(group.create().is_none(), "still picking members");
        group.next();
        group.set_name(" \u{200b} ");
        assert!(group.create().is_none());
        group.set_name(&"a".repeat(MAX_GROUP_TITLE + 1));
        assert!(!group.name_valid());
        assert!(group.create().is_none());

        group.set_name("Trip");
        assert!(matches!(
            group.create(),
            Some(Command::CreateGroup { title, members })
                if title == "Trip" && members == [id(1)]
        ));
        assert!(group.creating());
        assert!(group.create().is_none());
        assert!(!group.back(), "stays while creating");
    }

    #[test]
    fn a_failure_shows_until_tried_again() {
        let mut group = NewGroup::default();
        group.toggle(id(1));
        group.next();
        group.set_name("Trip");
        group.create();
        assert!(group.finished(Some("offline".into())));
        assert_eq!(group.error(), Some("offline"));
        assert!(!group.creating());
        assert!(group.create().is_some());
        assert_eq!(group.error(), None);
        assert!(!NewGroup::default().finished(Some("late".into())));
    }
}
