import { peppyCopy } from "../generated/peppyCopy";

export function ConversationRcsNotice({ historyGap = false }: { historyGap?: boolean }) {
  return <div id="conversation-rcs-notice" role="status" data-rcs-notice={historyGap ? "history-gap" : undefined}>{historyGap ? peppyCopy.rcs_history_gap_notice : ""}</div>;
}
