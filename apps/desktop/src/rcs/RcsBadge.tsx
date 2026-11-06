import { peppyCopy } from "../generated/peppyCopy";

export function RcsBadge({ encryptionReported = false }: { encryptionReported?: boolean }) {
  return <>
    <span id="conversation-rcs-badge" data-rcs-badge="current" title={peppyCopy.rcs_badge_description}>
      <span aria-hidden="true">{peppyCopy.rcs_badge_label}</span>
      <span className="visually-hidden">{peppyCopy.rcs_badge_label}. {peppyCopy.rcs_badge_description}</span>
    </span>
    {encryptionReported && <span id="conversation-rcs-e2ee-badge" data-rcs-e2ee-badge="reported" title={peppyCopy.rcs_e2ee_badge_description}>
      <span aria-hidden="true">{peppyCopy.rcs_e2ee_badge_label}</span>
      <span className="visually-hidden">{peppyCopy.rcs_e2ee_badge_label}. {peppyCopy.rcs_e2ee_badge_description}</span>
    </span>}
  </>;
}
