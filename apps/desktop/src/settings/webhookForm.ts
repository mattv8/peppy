import { formatPhoneNumber, normalizePhoneNumber, type RecipientChip } from "@peppy/desktop-ui";
import type { WebhookFilterEntry, WebhookFilterMode, WebhookInput, WebhookView } from "@peppy/desktop-ui/bridge-contracts";
import {
  FILTER_EMPTY_MESSAGE,
  FILTER_TOO_LARGE_MESSAGE,
  INVALID_URL_MESSAGE,
  MAX_FILTER_ENTRIES,
  MAX_NAME_LENGTH,
  NAME_TOO_LONG_MESSAGE,
} from "./webhookDisplay";

/** Sender metadata of a contact suggestion, keyed by the address the chip input reports. */
export type ChipSource = { address: string; displayName: string; contactId: string; bookId: string };

export type WebhookDraft = {
  name: string;
  url: string;
  filterMode: WebhookFilterMode;
  filter: WebhookFilterEntry[];
  paused: boolean;
};

export type WebhookDraftErrors = Partial<Record<"name" | "url" | "filter", string>>;

export function toWebhookInput(draft: WebhookDraft): WebhookInput {
  return {
    name: draft.name.trim() || null,
    url: draft.url.trim(),
    events: ["message.received"],
    filterMode: draft.filterMode,
    filter: draft.filterMode === "all" ? [] : draft.filter,
    paused: draft.paused,
  };
}

/** A full update body for a saved webhook; the server replaces the whole record. */
export function webhookInputFor(webhook: WebhookView, paused: boolean): WebhookInput {
  return toWebhookInput({ ...draftFromWebhook(webhook), paused });
}

/** The editable draft a saved webhook starts from; a derived name starts blank. */
export function draftFromWebhook(webhook: WebhookView): WebhookDraft {
  return {
    name: webhook.nameIsDerived ? "" : webhook.name,
    url: webhook.url,
    filterMode: webhook.filterMode,
    filter: webhook.filter,
    paused: webhook.paused,
  };
}

/** Comparable form of the editable fields; equal signatures mean no unsaved change. */
export function draftSignature(draft: WebhookDraft): string {
  return JSON.stringify({
    name: draft.name.trim(),
    url: draft.url.trim(),
    filterMode: draft.filterMode,
    filter: draft.filterMode === "all" ? [] : draft.filter,
  });
}

export function webhookUrlIsValid(value: string): boolean {
  try {
    const { protocol } = new URL(value.trim());
    return protocol === "https:" || protocol === "http:";
  } catch {
    return false;
  }
}

export function webhookDraftErrors(draft: WebhookDraft): WebhookDraftErrors {
  const errors: WebhookDraftErrors = {};
  if (draft.name.trim().length > MAX_NAME_LENGTH) errors.name = NAME_TOO_LONG_MESSAGE;
  if (!webhookUrlIsValid(draft.url)) errors.url = INVALID_URL_MESSAGE;
  if (draft.filterMode !== "all") {
    if (draft.filter.length === 0) errors.filter = FILTER_EMPTY_MESSAGE;
    else if (draft.filter.length > MAX_FILTER_ENTRIES) errors.filter = FILTER_TOO_LARGE_MESSAGE;
  }
  return errors;
}

export function filterRecipientChips(entries: WebhookFilterEntry[]): RecipientChip[] {
  return entries.map((entry) => ({
    id: entry.address,
    label: entry.label ?? formatPhoneNumber(entry.address),
  }));
}

const canonicalAddress = (value: string) => normalizePhoneNumber(value) ?? value.trim();

/**
 * Maps the chip input's committed ids back to filter entries. Existing entries keep their
 * contact metadata; contact suggestions gain it; typed numbers carry only the address.
 */
export function filterEntriesFromChipIds(
  ids: string[],
  previous: WebhookFilterEntry[],
  sources: ReadonlyMap<string, ChipSource>,
): WebhookFilterEntry[] {
  return ids.flatMap((id) => {
    const existing = previous.find((entry) => entry.address === id);
    if (existing) return [existing];
    const source = sources.get(id);
    if (source) {
      return [{
        address: canonicalAddress(source.address),
        label: source.displayName,
        contactId: source.contactId,
        bookId: source.bookId,
      }];
    }
    const address = canonicalAddress(id);
    return address ? [{ address, label: null, contactId: null, bookId: null }] : [];
  });
}
