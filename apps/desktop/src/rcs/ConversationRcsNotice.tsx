import { peppyCopy } from "../generated/peppyCopy";

export type GroupNoticeState = "live" | "paused" | "mms" | "left";

/**
 * One status message at a time, in fixed priority: left, paused, then history gap. The live region
 * stays mounted so announcements land reliably; it is empty when there is nothing to say.
 */
export function ConversationRcsNotice({ historyGap = false, group }: { historyGap?: boolean; group?: { state: GroupNoticeState } }) {
  const notice = groupNotice(group?.state) ?? (historyGap ? { kind: "history-gap", text: peppyCopy.rcs_history_gap_notice } : undefined);
  return <div id="conversation-rcs-notice" role="status" data-rcs-notice={notice?.kind}>{notice?.text ?? ""}</div>;
}

function groupNotice(state: GroupNoticeState | undefined): { kind: string; text: string } | undefined {
  if (state === "left") return { kind: "group-left", text: peppyCopy.group_notice_left };
  if (state === "paused") return { kind: "group-paused", text: peppyCopy.group_notice_paused };
  return undefined;
}
