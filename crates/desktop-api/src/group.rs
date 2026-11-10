//! Desktop view of RCS group conversations: the group projection, its notices, and the refusal
//! payload attached to a refused group send. Canonical addresses only; no Google identifiers.
use peppy_client_core::{
    Client, ConversationId, Error, GroupLinkState, GroupNotice, GroupNoticeKind,
    GroupView as CoreGroupView,
};
use serde::Serialize;

#[derive(Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct GroupView {
    /// Omitted when the group has no name; an empty name is never sent.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
    /// Other members, canonical E.164, without the user's own addresses.
    pub members: Vec<String>,
    pub state: &'static str,
    /// Decimal `u64`; the composer echoes it back as `expectedMemberRevision`.
    pub member_revision: String,
    pub can_create: bool,
    pub create_needs_confirmation: bool,
    pub notices: Vec<GroupNoticeView>,
}

#[derive(Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct GroupNoticeView {
    pub kind: &'static str,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub member: Option<String>,
    /// Decimal epoch milliseconds.
    pub at_ms: String,
}

/// Members and revision a refused group send reports, so the composer can show the new list.
#[derive(Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct GroupRefusalView {
    pub members: Vec<String>,
    pub member_revision: String,
}

/// Group projection for a conversation; `None` when it is not a group.
pub fn conversation_group_view(
    client: &Client,
    conversation_id: ConversationId,
) -> Result<Option<GroupView>, Error> {
    let Some(view) = client.group_view(conversation_id)? else {
        return Ok(None);
    };
    let notices = client.group_notices(conversation_id)?;
    Ok(Some(group_view(view, &notices)))
}

fn group_view(view: CoreGroupView, notices: &[GroupNotice]) -> GroupView {
    GroupView {
        name: view.name.filter(|name| !name.is_empty()),
        members: view.members,
        state: group_state_label(view.state),
        member_revision: view.member_revision.to_string(),
        can_create: view.can_create,
        create_needs_confirmation: view.create_needs_confirmation,
        notices: notices.iter().filter_map(notice_view).collect(),
    }
}

pub fn group_state_label(state: GroupLinkState) -> &'static str {
    match state {
        GroupLinkState::Unlinked => "unlinked",
        GroupLinkState::Proving => "proving",
        GroupLinkState::Live => "live",
        GroupLinkState::Paused => "paused",
        GroupLinkState::Withdrawn => "withdrawn",
    }
}

/// One notice per member change. A member notice without its member is dropped, never guessed.
fn notice_view(notice: &GroupNotice) -> Option<GroupNoticeView> {
    let kind = match notice.kind {
        GroupNoticeKind::Renamed => "renamed",
        GroupNoticeKind::Added if notice.member.is_some() => "added",
        GroupNoticeKind::Removed if notice.member.is_some() => "removed",
        GroupNoticeKind::Added | GroupNoticeKind::Removed => return None,
    };
    let is_member_notice = kind != "renamed";
    Some(GroupNoticeView {
        kind,
        name: notice.name.clone().filter(|name| !name.is_empty()),
        member: notice.member.clone().filter(|_| is_member_notice),
        at_ms: u64::try_from(notice.at_ms).unwrap_or(0).to_string(),
    })
}

/// Refusal payload for a group send refused with `members_changed`: the current list and revision.
pub fn group_refusal_view(
    client: &Client,
    conversation_id: ConversationId,
) -> Result<Option<GroupRefusalView>, Error> {
    Ok(client
        .group_view(conversation_id)?
        .map(|view| GroupRefusalView {
            members: view.members,
            member_revision: view.member_revision.to_string(),
        }))
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn core_view(name: Option<&str>, state: GroupLinkState) -> CoreGroupView {
        CoreGroupView {
            name: name.map(str::to_owned),
            members: vec!["+15551230001".into(), "+15551230002".into()],
            state,
            member_revision: 18_446_744_073_709_551_615,
            can_create: false,
            create_needs_confirmation: true,
        }
    }

    fn notice(kind: GroupNoticeKind, name: Option<&str>, member: Option<&str>) -> GroupNotice {
        GroupNotice {
            kind,
            name: name.map(str::to_owned),
            member: member.map(str::to_owned),
            at_ms: 1_730_000_000_000,
        }
    }

    #[test]
    fn group_view_serializes_camel_case_with_decimal_revision() {
        let value = serde_json::to_value(group_view(
            core_view(Some("Weekend"), GroupLinkState::Live),
            &[],
        ))
        .unwrap();

        assert_eq!(
            value,
            json!({
                "name": "Weekend",
                "members": ["+15551230001", "+15551230002"],
                "state": "live",
                "memberRevision": "18446744073709551615",
                "canCreate": false,
                "createNeedsConfirmation": true,
                "notices": [],
            })
        );
    }

    #[test]
    fn unnamed_and_empty_names_are_omitted() {
        let unnamed =
            serde_json::to_value(group_view(core_view(None, GroupLinkState::Paused), &[])).unwrap();
        let empty = serde_json::to_value(group_view(
            core_view(Some(""), GroupLinkState::Unlinked),
            &[],
        ))
        .unwrap();

        assert!(unnamed.get("name").is_none());
        assert!(empty.get("name").is_none());
        assert_eq!(unnamed["state"], "paused");
        assert_eq!(empty["state"], "unlinked");
    }

    #[test]
    fn every_link_state_has_the_contract_label() {
        let labels: Vec<_> = [
            GroupLinkState::Unlinked,
            GroupLinkState::Proving,
            GroupLinkState::Live,
            GroupLinkState::Paused,
            GroupLinkState::Withdrawn,
        ]
        .into_iter()
        .map(group_state_label)
        .collect();

        assert_eq!(
            labels,
            ["unlinked", "proving", "live", "paused", "withdrawn"]
        );
    }

    #[test]
    fn notices_map_kinds_and_drop_member_notices_without_a_member() {
        let notices = [
            notice(GroupNoticeKind::Renamed, Some("New name"), None),
            notice(GroupNoticeKind::Renamed, None, None),
            notice(GroupNoticeKind::Added, None, Some("+15551230003")),
            notice(GroupNoticeKind::Removed, None, Some("+15551230004")),
            notice(GroupNoticeKind::Added, None, None),
        ];
        let value = serde_json::to_value(group_view(
            core_view(Some("Weekend"), GroupLinkState::Live),
            &notices,
        ))
        .unwrap();

        assert_eq!(
            value["notices"],
            json!([
                { "kind": "renamed", "name": "New name", "atMs": "1730000000000" },
                { "kind": "renamed", "atMs": "1730000000000" },
                { "kind": "added", "member": "+15551230003", "atMs": "1730000000000" },
                { "kind": "removed", "member": "+15551230004", "atMs": "1730000000000" },
            ])
        );
    }

    #[test]
    fn renamed_notice_never_carries_a_member() {
        let value = notice_view(&notice(
            GroupNoticeKind::Renamed,
            Some("Name"),
            Some("+15551230009"),
        ))
        .unwrap();

        assert_eq!(value.kind, "renamed");
        assert_eq!(value.member, None);
    }

    #[test]
    fn refusal_view_serializes_the_current_roster() {
        let value = serde_json::to_value(GroupRefusalView {
            members: vec!["+15551230001".into()],
            member_revision: "7".into(),
        })
        .unwrap();

        assert_eq!(
            value,
            json!({ "members": ["+15551230001"], "memberRevision": "7" })
        );
    }
}
