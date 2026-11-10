import { peppyCopy } from "../generated/peppyCopy";

/** Shown while the first send creates the RCS group in Google Messages. */
export function GroupCreateProgress() {
  return <p id="composer-group-create-status" role="status">{peppyCopy.group_create_progress}</p>;
}

/** The first send may have created a group, so the user decides whether to create another. */
export function GroupCreateUnconfirmed({ onCreateAgain }: { onCreateAgain(): void }) {
  return <div id="composer-group-create-unconfirmed" role="alert" data-group-create="unconfirmed">
    <p>{peppyCopy.group_create_again_confirm}</p>
    <button type="button" id="group-create-again" className="secondary-button" onClick={onCreateAgain}>{peppyCopy.group_create_again_action}</button>
  </div>;
}

/** The message went out as MMS because no RCS group could be made. The name was only kept in Peppy. */
export function GroupCreateNotRcsInfo() {
  return <p id="composer-group-create-not-rcs" role="status" data-group-create="not-rcs">{peppyCopy.group_create_not_rcs_info}</p>;
}

/** A stale Peppy device tried to send MMS into an RCS group. Only an update fixes it. */
export function GroupUpdateRequiredNotice() {
  return <p id="composer-group-update-required" role="status" data-group-send-refusal="update-required">{peppyCopy.group_update_required}</p>;
}

/** Peppy can no longer send to this group. The user can start a new MMS group with the same people. */
export function GroupWithdrawnNotice({ onStartNewMms }: { onStartNewMms(): void }) {
  return <div id="composer-group-withdrawn" role="status" data-group-send-refusal="withdrawn">
    <p>{peppyCopy.group_withdrawn_notice}</p>
    <button type="button" id="group-start-new-mms" className="secondary-button" onClick={onStartNewMms}>{peppyCopy.group_withdrawn_action}</button>
  </div>;
}
