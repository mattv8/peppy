import { formatPhoneNumber, seedKey, type RecipientSuggestion } from "@peppy/desktop-ui";
import { bridge } from "../bridge/bridge";

export type RecipientSource = Awaited<ReturnType<typeof bridge.search_contact_recipients>>[number];
export type RecipientSuggestionWithSource = RecipientSuggestion & { source: RecipientSource };

export async function searchRecipientSuggestions(
  query: string,
  sourceDeviceId?: string,
): Promise<RecipientSuggestionWithSource[]> {
  const found = await bridge.search_contact_recipients(query, sourceDeviceId);
  return found.map((source) => {
    return {
      id: source.address,
      rowId: JSON.stringify([source.bookId, source.contactId, source.phoneId]),
      label: source.displayName,
      detail: [source.label, source.normalized ? formatPhoneNumber(source.address) : source.number]
        .filter(Boolean)
        .join(" · "),
      avatarUrl: source.avatarUrl,
      avatarSeed: seedKey(source.address),
      avatarName: source.displayName,
      source,
    };
  });
}

export function recipientSuggestionSource(suggestion: RecipientSuggestion): RecipientSource | null {
  return "source" in suggestion ? (suggestion as RecipientSuggestionWithSource).source : null;
}
