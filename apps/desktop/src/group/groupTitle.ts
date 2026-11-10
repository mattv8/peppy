import { peppyCopy } from "../generated/peppyCopy";
import { fillCopy } from "./groupCopy";
import { displayGroupName } from "./groupName";

export type GroupTitleInput = {
  name?: string;
  /** Other participants' display names, in roster order. Excludes the user. */
  memberNames: readonly string[];
};

/** Header title: the group name when set, otherwise the member names, otherwise "Group". */
export function groupTitle({ name, memberNames }: GroupTitleInput): string {
  return displayGroupName(name) ?? (memberNames.join(", ") || peppyCopy.group_tag_group);
}

/**
 * Header sub-line. A named group lists its members; an unnamed group shows a count, because the
 * title already is the member list. The count includes the user.
 */
export function groupSubtitle({ name, memberNames }: GroupTitleInput): string {
  if (displayGroupName(name)) return memberNames.join(", ");
  return fillCopy(peppyCopy.group_header_count, { count: memberNames.length + 1 });
}
