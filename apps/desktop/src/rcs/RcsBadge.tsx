import { peppyCopy } from "../generated/peppyCopy";

/** `paused` keeps the RCS badge while a group's members are being re-checked; the status bar explains it. */
export function RcsBadge({ encryptionReported = false, paused = false }: { encryptionReported?: boolean; paused?: boolean }) {
  return <>
    <span id="conversation-rcs-badge" data-rcs-badge="current" data-paused={paused || undefined} title={peppyCopy.rcs_badge_description}>
      <span aria-hidden="true">{peppyCopy.rcs_badge_label}</span>
      <span className="visually-hidden">{peppyCopy.rcs_badge_label}. {peppyCopy.rcs_badge_description}</span>
    </span>
    {encryptionReported && <span id="conversation-rcs-e2ee-badge" data-rcs-e2ee-badge="reported" title={peppyCopy.rcs_e2ee_badge_description}>
      <span aria-hidden="true">{peppyCopy.rcs_e2ee_badge_label}</span>
      <span className="visually-hidden">{peppyCopy.rcs_e2ee_badge_label}. {peppyCopy.rcs_e2ee_badge_description}</span>
    </span>}
  </>;
}

export function MmsGroupBadge() {
  return <span id="conversation-mms-badge" data-rcs-badge="mms" title={peppyCopy.group_badge_mms_description}>
    <span aria-hidden="true">{peppyCopy.group_badge_mms_label}</span>
    <span className="visually-hidden">{peppyCopy.group_badge_mms_label}. {peppyCopy.group_badge_mms_description}</span>
  </span>;
}
