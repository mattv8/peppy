import type { ConversationView, GroupView, SendResultView } from "@peppy/desktop-ui/bridge-contracts";
import type { GroupNoticeState } from "../rcs/ConversationRcsNotice";

/** Status-bar state for a group, or undefined when the bar says nothing about the group. */
export function groupNoticeState(group: Pick<GroupView, "state"> | undefined): GroupNoticeState | undefined {
  if (group?.state === "withdrawn") return "left";
  if (group?.state === "paused") return "paused";
  return undefined;
}

/** A withdrawn group is read-only: the user can still read it but cannot send to it. */
export const groupIsReadOnly = (group: Pick<GroupView, "state"> | undefined): boolean => group?.state === "withdrawn";

/** Typing is sent in a group only while the group is live (UX spec §5). One-to-one chats are unchanged. */
export const groupAllowsTypingSend = (conversation: Pick<ConversationView, "participants" | "group"> | undefined): boolean =>
  (conversation?.participants?.length ?? 0) <= 1 || conversation?.group?.state === "live";

/** Members for a group's visible roster. The group projection wins; otherwise the native participants. */
export const groupMembers = (conversation: Pick<ConversationView, "participants" | "group">): string[] =>
  conversation.group?.members ?? conversation.participants ?? [];

/** Refusal states the composer shows, mapped from a failed or accepted send result. */
export type GroupComposerNotice =
  | { kind: "members-changed"; key: string; memberRevision?: string; members?: string[] }
  | { kind: "create-unconfirmed"; key: string }
  | { kind: "create-not-rcs"; key: string }
  | { kind: "update-required"; key: string }
  | { kind: "withdrawn"; key: string };

/** Maps a send result's reason to the composer notice it needs; undefined for reasons with generic copy. */
export function groupComposerNotice(result: Pick<SendResultView, "reason" | "group">, key: string): GroupComposerNotice | undefined {
  switch (result.reason) {
    case "members_changed":
      return { kind: "members-changed", key, memberRevision: result.group?.memberRevision, members: result.group?.members };
    case "create_unconfirmed":
      return { kind: "create-unconfirmed", key };
    case "create_not_rcs":
      return { kind: "create-not-rcs", key };
    case "update_required":
      return { kind: "update-required", key };
    case "withdrawn":
      return { kind: "withdrawn", key };
    default:
      return undefined;
  }
}
