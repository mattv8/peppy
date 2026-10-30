import { isValidPhoneNumber } from "@peppy/desktop-ui";
import type { ContactBookView, ContactResolutionMap, ConversationView } from "./bridge";
import { senderLabel } from "./sender/attribution";

export type ConversationContactAddressInput = {
  conversation: ConversationView | undefined;
  resolution: ContactResolutionMap;
  books: ContactBookView[] | undefined;
};

export function canCreateContactInBook(book: ContactBookView): boolean {
  return book.capabilities.canWrite
    && !book.sourceRevoked
    && (book.state === "active" || book.state === "limited");
}

export function conversationContactAddress({
  conversation,
  resolution,
  books,
}: ConversationContactAddressInput): string | undefined {
  const address = conversation?.participants?.length === 1
    ? conversation.participants[0]
    : undefined;

  if (
    !address
    || conversation?.draftOnly
    || !isValidPhoneNumber(address)
    || senderLabel(address, resolution).contact
    || !books?.some(canCreateContactInBook)
  ) return undefined;

  return address;
}
