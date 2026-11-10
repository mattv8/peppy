import type { RefObject } from "react";
import { ContactAvatar, Modal, formatPhoneNumber, incomingToneSeed } from "@peppy/desktop-ui";
import type { GroupView } from "@peppy/desktop-ui/bridge-contracts";
import { peppyCopy } from "../generated/peppyCopy";
import { fillCopy } from "./groupCopy";
import { displayGroupName } from "./groupName";

export type GroupDetailsState = Pick<GroupView, "name" | "members" | "state">;

export type GroupDetailsDialogProps = {
  group: GroupDetailsState;
  /** Contact name for a canonical address, or undefined when no contact matches. */
  contactName: (address: string) => string | undefined;
  /** Seed for member avatars, the same one the conversation's bubbles use. */
  toneSeed: string;
  /** Called after the dialog closes; opens the member's contact (or the add-contact flow). */
  onOpenContact(address: string): void;
  onDismiss(): void;
  opener?: RefObject<HTMLElement | null>;
};

/** How the group is delivered, as the details dialog describes it. `withdrawn` reads as "left" (contracts §7). */
export function groupConnection(state: GroupDetailsState["state"]): "rcs" | "paused" | "mms" | "left" {
  if (state === "live") return "rcs";
  if (state === "paused") return "paused";
  if (state === "withdrawn") return "left";
  return "mms";
}

const connectionCopy = {
  rcs: peppyCopy.group_details_connection_rcs,
  paused: peppyCopy.group_details_connection_paused,
  mms: peppyCopy.group_details_connection_mms,
  left: peppyCopy.group_details_connection_left,
};

/**
 * Sorts members by contact name, A to Z, then unresolved numbers by number. The order is stable
 * and there is no reorder control.
 */
export function orderGroupMembers(addresses: readonly string[], contactName: (address: string) => string | undefined): string[] {
  return [...addresses].sort((a, b) => {
    const nameA = contactName(a);
    const nameB = contactName(b);
    if (nameA && nameB) return nameA.localeCompare(nameB, undefined, { sensitivity: "base" });
    if (nameA) return -1;
    if (nameB) return 1;
    return a.localeCompare(b, undefined, { numeric: true });
  });
}

export function GroupDetailsDialog({ group, contactName, toneSeed, onOpenContact, onDismiss, opener }: GroupDetailsDialogProps) {
  const connection = groupConnection(group.state);
  const name = displayGroupName(group.name);
  const left = connection === "left";
  const members = orderGroupMembers(group.members, contactName);
  const showPointer = connection === "rcs" || connection === "paused";
  return <Modal id="group-details-dialog" title={peppyCopy.group_details_title} opener={opener} onDismiss={onDismiss} actions={<button type="button" id="group-details-close" className="secondary-button" onClick={onDismiss}>{peppyCopy.help_dismiss}</button>}>
    <div id="group-details-body">
      <dl id="group-details-facts">
        <div data-group-fact="name">
          <dt>{peppyCopy.group_details_name_label}</dt>
          <dd>{name ? <bdi>{name}</bdi> : <span className="group-details-muted">{peppyCopy.group_details_name_none}</span>}</dd>
        </div>
        <div data-group-fact="connection" data-group-connection={connection}>
          <dt>{peppyCopy.group_details_connection_label}</dt>
          <dd>{connectionCopy[connection]}</dd>
        </div>
      </dl>
      {connection === "paused" && <p id="group-details-paused" role="status">{peppyCopy.group_notice_paused}</p>}
      {showPointer && <p id="group-details-google-pointer">{peppyCopy.group_details_google_pointer}</p>}
      <h3 id="group-members-heading">{peppyCopy.group_details_members_heading} <span>{left ? members.length : members.length + 1}</span></h3>
      {members.length === 0 && !left
        ? <p id="group-members-empty">{peppyCopy.group_details_members_empty}</p>
        : <ul id="group-members-list">
          {!left && <li id="group-member-self" data-member-self="true" className="group-member-self">{peppyCopy.group_details_you}</li>}
          {members.map((address) => <MemberRow key={address} address={address} contactName={contactName} toneSeed={toneSeed} onOpenContact={() => { onDismiss(); onOpenContact(address); }} />)}
        </ul>}
    </div>
  </Modal>;
}

function MemberRow({ address, contactName, toneSeed, onOpenContact }: { address: string; contactName: (address: string) => string | undefined; toneSeed: string; onOpenContact(): void }) {
  const name = contactName(address);
  const number = formatPhoneNumber(address);
  const label = name ? fillCopy(peppyCopy.group_member_open_contact, { name }) : fillCopy(peppyCopy.group_member_add_contact, { name: number });
  return <li id={`group-member-${address}`} data-member-address={address}>
    <button type="button" className="group-member-row" aria-label={label} onClick={onOpenContact}>
      <ContactAvatar seed={incomingToneSeed(toneSeed, address)} name={name ?? number} size={24} className="group-member-avatar" />
      <span className="group-member-text">
        <bdi>{name ?? number}</bdi>
        {name && <small>{number}</small>}
      </span>
    </button>
  </li>;
}
