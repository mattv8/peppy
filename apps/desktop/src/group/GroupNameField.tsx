import { peppyCopy } from "../generated/peppyCopy";
import { GROUP_NAME_MAX_LENGTH } from "./groupName";

type GroupNameFieldProps = {
  value: string;
  /** Raw input. Normalize with `normalizeGroupName` when the draft is saved or sent. */
  onChange(value: string): void;
  recipientCount: number;
  rcsLinked: boolean;
};

/**
 * Optional group name for a new conversation with two or more recipients. Renders nothing for a
 * one-to-one draft. The help line says whether the name can reach Google Messages.
 */
export function GroupNameField({ value, onChange, recipientCount, rcsLinked }: GroupNameFieldProps) {
  if (recipientCount < 2) return null;
  const label = peppyCopy.group_name_label;
  return <>
    <label id="new-group-name-row" className="recipient-group-name">
      <span aria-hidden="true">{peppyCopy.group_name_row_label}</span>
      <input id="new-group-name" type="text" maxLength={GROUP_NAME_MAX_LENGTH} aria-label={label} placeholder={label} autoComplete="off" aria-describedby="new-group-help" value={value} onChange={(event) => onChange(event.currentTarget.value)} />
    </label>
    <p id="new-group-help" className="recipient-chip-hint" data-group-name-help={rcsLinked ? "rcs" : "mms"}>
      {rcsLinked ? peppyCopy.group_name_help_rcs : peppyCopy.group_name_help_mms}
    </p>
  </>;
}
