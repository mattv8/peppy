import { formatPhoneNumber, seedKey, type RecipientSuggestion } from "@peppy/desktop-ui";
import { bridge } from "../bridge/bridge";

export type RecipientSource = Awaited<ReturnType<typeof bridge.search_contact_recipients>>[number];
/** `source` is null when rows merged from different contacts (for example one person in two books) name no single contact. */
export type RecipientSuggestionWithSource = RecipientSuggestion & { source: RecipientSource | null };

const recipientGroupKey = (row: RecipientSource) => JSON.stringify([row.address, row.displayName, row.label ?? ""]);

const recipientRowId = (row: RecipientSource) => JSON.stringify([row.bookId, row.contactId, row.phoneId]);

const isSameContact = (left: RecipientSource, right: RecipientSource) => left.bookId === right.bookId && left.contactId === right.contactId;

/** Groups rows with the same address, name and label in one pass, keeping first-seen order. */
function groupRecipientRows(rows: RecipientSource[]): Map<string, RecipientSource[]> {
  const groups = new Map<string, RecipientSource[]>();
  for (const row of rows) {
    const key = recipientGroupKey(row);
    const group = groups.get(key);
    if (group) group.push(row);
    else groups.set(key, [row]);
  }
  return groups;
}

function suggestionFromGroup(key: string, rows: RecipientSource[]): RecipientSuggestionWithSource {
  const [first] = rows;
  const sharesContact = rows.every((row) => isSameContact(row, first));
  return {
    id: first.address,
    rowId: rows.length === 1 ? recipientRowId(first) : key,
    label: first.displayName,
    detail: [first.label, first.normalized ? formatPhoneNumber(first.address) : first.number]
      .filter(Boolean)
      .join(" · "),
    avatarUrl: first.avatarUrl,
    avatarSeed: seedKey(first.address),
    avatarName: first.displayName,
    source: sharesContact ? first : null,
  };
}

export async function searchRecipientSuggestions(
  query: string,
  sourceDeviceId?: string,
): Promise<RecipientSuggestionWithSource[]> {
  const found = await bridge.search_contact_recipients(query, sourceDeviceId);
  return Array.from(groupRecipientRows(found), ([key, rows]) => suggestionFromGroup(key, rows));
}

export function recipientSuggestionSource(suggestion: RecipientSuggestion): RecipientSource | null {
  return "source" in suggestion ? (suggestion as RecipientSuggestionWithSource).source : null;
}
