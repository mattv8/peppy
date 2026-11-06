import { peppyCopy } from "../generated/peppyCopy";

export function RcsBadge() {
  return <span id="conversation-rcs-badge" data-rcs-badge="current" title={peppyCopy.rcs_badge_description}>
    <span aria-hidden="true">{peppyCopy.rcs_badge_label}</span>
    <span className="visually-hidden">{peppyCopy.rcs_badge_label}. {peppyCopy.rcs_badge_description}</span>
  </span>;
}
