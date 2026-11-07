import { formatPhoneNumber, seedKey, type RecipientSuggestion } from "@peppy/desktop-ui";
import { bridge } from "../bridge/bridge";

export type RecipientSource = Awaited<ReturnType<typeof bridge.search_contact_recipients>>[number];

export async function searchRecipientSuggestions(
  query: string,
  sourceDeviceId?: string,
  sources?: Map<string, RecipientSource>,
): Promise<RecipientSuggestion[]> {
  const found = await bridge.search_contact_recipients(query, sourceDeviceId);
  return found.map((match) => {
    sources?.set(match.address, match);
    return {
      id: match.address,
      rowId: JSON.stringify([match.bookId, match.contactId, match.phoneId]),
      label: match.displayName,
      detail: [match.label, match.normalized ? formatPhoneNumber(match.address) : match.number]
        .filter(Boolean)
        .join(" · "),
      avatarUrl: match.avatarUrl,
      avatarSeed: seedKey(match.address),
      avatarName: match.displayName,
    };
  });
}
