import { peppyCopy } from "../generated/peppyCopy";
import { fillCopy, summarizeGroupNames } from "./groupCopy";

type GroupMembersChangedNoticeProps = {
  memberRevision: string;
  /** Current members once the metadata has arrived. Undefined shows only the generic line. */
  members?: readonly string[];
  displayName: (address: string) => string;
  /** False when the group can no longer take this send (for example it went MMS or left). */
  canResend: boolean;
  /** True while the group is being re-checked. The resend action is held until it settles. */
  checking: boolean;
  onResend(): void;
  onReview(): void;
};

/**
 * Refusal shown above the composer after a send was not sent because the members changed.
 * Nothing is sent again without the user pressing one of its actions.
 */
export function GroupMembersChangedNotice({ memberRevision, members, displayName, canResend, checking, onResend, onReview }: GroupMembersChangedNoticeProps) {
  const hasMembers = members !== undefined && members.length > 0;
  return <div id="composer-group-changed" role="alert" data-group-send-refusal="members-changed" data-member-revision={memberRevision}>
    <p className="composer-group-changed-title">{peppyCopy.group_send_members_changed}</p>
    {hasMembers && <p id="composer-group-changed-members">{fillCopy(peppyCopy.group_send_members_now, { names: summarizeGroupNames(members.map(displayName)) })}</p>}
    <div className="composer-group-changed-actions">
      {canResend && <button type="button" id="group-send-updated" className="secondary-button" aria-disabled={checking || undefined} onClick={() => { if (!checking) onResend(); }}>
        {checking ? peppyCopy.group_send_checking : peppyCopy.group_send_resend}
      </button>}
      <button type="button" id="group-send-review" className="secondary-button" onClick={onReview}>{peppyCopy.group_send_review}</button>
    </div>
  </div>;
}
