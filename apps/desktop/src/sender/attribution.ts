import { formatPhoneNumber } from "@peppy/desktop-ui";
import type { ContactResolutionMap } from "../bridge";

export type SenderLabel = {
  text: string;
  isName: boolean;
  contact?: { bookId: string; contactId: string };
};

export type SenderRunIdentity =
  | { kind: "outgoing" }
  | { kind: "boundary" }
  | { kind: "incoming"; address?: string | null };

export type SenderRunOptions<T> = {
  key(item: T): string;
  identity(item: T): SenderRunIdentity;
  endsRunAfter(item: T): boolean;
};

export function senderLabel(address: string | undefined, resolution: ContactResolutionMap): SenderLabel {
  if (!address) return { text: "Unknown sender", isName: false };

  const contact = resolution[address];
  if (!contact) return { text: formatPhoneNumber(address), isName: false };

  if (contact.bookId && contact.contactId) {
    return {
      text: contact.displayName,
      isName: true,
      contact: { bookId: contact.bookId, contactId: contact.contactId },
    };
  }

  return { text: contact.displayName, isName: true };
}

export function senderRunEdges<T>(
  items: readonly T[],
  { key, identity, endsRunAfter }: SenderRunOptions<T>,
): Map<string, { start: boolean; end: boolean }> {
  const edges = new Map<string, { start: boolean; end: boolean }>();

  for (let index = 0; index < items.length; index += 1) {
    const item = items[index];
    const itemIdentity = identity(item);
    if (itemIdentity.kind !== "incoming") continue;

    const previous = index > 0 ? items[index - 1] : undefined;
    const next = items[index + 1];
    edges.set(key(item), {
      start: !continuesSenderRun(previous, item, identity, endsRunAfter),
      end: !continuesSenderRun(item, next, identity, endsRunAfter),
    });
  }

  return edges;
}

function continuesSenderRun<T>(
  previous: T | undefined,
  next: T | undefined,
  identity: (item: T) => SenderRunIdentity,
  endsRunAfter: (item: T) => boolean,
): boolean {
  if (previous === undefined || next === undefined || endsRunAfter(previous)) return false;

  const previousIdentity = identity(previous);
  const nextIdentity = identity(next);
  return (
    previousIdentity.kind === "incoming" &&
    nextIdentity.kind === "incoming" &&
    Boolean(previousIdentity.address) &&
    previousIdentity.address === nextIdentity.address
  );
}
