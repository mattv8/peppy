import { peppyCopy } from "../generated/peppyCopy";

export type CopySegment = { text: string; value: boolean; offset: number };

const PLACEHOLDER = /\{(\w+)\}/g;

/**
 * Splits a catalog template into literal and filled segments. Tokens without a value stay literal,
 * so a missing value is visible in the output instead of silently disappearing.
 */
export function copySegments(template: string, values: Record<string, string | number>): CopySegment[] {
  const segments: CopySegment[] = [];
  let literalStart = 0;
  for (const match of template.matchAll(PLACEHOLDER)) {
    const [token, key] = match;
    if (!(key in values)) continue;
    const index = match.index ?? 0;
    if (index > literalStart) segments.push({ text: template.slice(literalStart, index), value: false, offset: literalStart });
    segments.push({ text: String(values[key]), value: true, offset: index });
    literalStart = index + token.length;
  }
  if (literalStart < template.length) segments.push({ text: template.slice(literalStart), value: false, offset: literalStart });
  return segments;
}

export function fillCopy(template: string, values: Record<string, string | number>): string {
  return copySegments(template, values).map((segment) => segment.text).join("");
}

/** Joins up to `limit` names with commas and appends the overflow count, per `group_send_members_more`. */
export function summarizeGroupNames(names: readonly string[], limit = 5): string {
  const shown = names.slice(0, limit).join(", ");
  const extra = names.length - limit;
  return extra > 0 ? fillCopy(peppyCopy.group_send_members_more, { names: shown, count: extra }) : shown;
}

/** Typing label for the people currently typing in a group. Empty when nobody is typing. */
export function groupTypingLabel(participants: readonly string[], displayName: (address: string) => string): string {
  if (participants.length === 0) return "";
  if (participants.length === 1) return fillCopy(peppyCopy.typing_indicator_group_one, { name: displayName(participants[0]) });
  if (participants.length === 2) return fillCopy(peppyCopy.typing_indicator_group_two, { first: displayName(participants[0]), second: displayName(participants[1]) });
  return fillCopy(peppyCopy.typing_indicator_group_many, { count: participants.length });
}
