export const AVATAR_PALETTE_SIZE = 120;

export type AvatarKind = "person" | "group";

export type AvatarIdentity = {
  seed: string;
  kind: AvatarKind;
  photoDataUrl?: string;
  name?: string;
};

const ASCII_WHITESPACE = /[ \t\n\r]+/g;
const PHONE_ADDRESS = /^\+?[0-9 ().-]+$/;
const ASCII_UPPERCASE = /[A-Z]/g;
const NON_DIGITS = /[^0-9]/g;
const UNICODE_WHITESPACE = /\p{White_Space}+/u;
const UNICODE_ALPHABETIC = /\p{Alphabetic}/u;
const textEncoder = new TextEncoder();

/** Normalizes addresses into stable phone or ASCII-case-insensitive identity keys. */
export function seedKey(address: string): string {
  const trimmed = address.replace(ASCII_WHITESPACE, " ").replace(/^ | $/g, "");
  if (!trimmed) {
    return "";
  }

  if (!PHONE_ADDRESS.test(trimmed)) {
    return trimmed.replace(ASCII_UPPERCASE, (character) => character.toLowerCase());
  }

  const digits = trimmed.replace(NON_DIGITS, "");
  if (!digits) {
    return trimmed;
  }
  if (digits.length >= 7) {
    if (trimmed.startsWith("+")) {
      return `+${digits}`;
    }
    if (digits.length === 10) {
      return `+1${digits}`;
    }
    if (digits.length === 11 && digits[0] === "1") {
      return `+${digits}`;
    }
  }
  return digits;
}

/** Returns the unsigned 32-bit FNV-1a hash of UTF-8 encoded text. */
export function fnv1a32(value: string): number {
  let hash = 0x811c9dc5;
  for (const byte of textEncoder.encode(value)) {
    hash ^= byte;
    hash = Math.imul(hash, 0x01000193) >>> 0;
  }
  return hash >>> 0;
}

export function paletteIndex(seed: string): number {
  return fnv1a32(seed) % AVATAR_PALETTE_SIZE;
}

function compareUtf8(left: string, right: string): number {
  const leftBytes = textEncoder.encode(left);
  const rightBytes = textEncoder.encode(right);
  const sharedLength = Math.min(leftBytes.length, rightBytes.length);

  for (let index = 0; index < sharedLength; index += 1) {
    const difference = leftBytes[index] - rightBytes[index];
    if (difference !== 0) {
      return difference;
    }
  }
  return leftBytes.length - rightBytes.length;
}

export function conversationAvatar(input: {
  id: string;
  name: string;
  participants?: readonly string[];
  recipientless?: boolean;
}): AvatarIdentity {
  if (input.recipientless) {
    return { seed: `conversation:${input.id}`, kind: "person" };
  }

  const addresses = input.participants?.length ? input.participants : input.name.split(", ");
  const keys = [...new Set(addresses.map(seedKey).filter(Boolean))].sort(compareUtf8);

  if (keys.length === 0) {
    return { seed: `conversation:${input.id}`, kind: "person" };
  }
  if (keys.length > 1) {
    return { seed: `group:${keys.join(",")}`, kind: "group" };
  }
  return { seed: keys[0], kind: "person" };
}

export function contactSeed(
  phones: readonly { number: string }[] | undefined,
  contactId: string,
): string {
  for (const phone of phones ?? []) {
    const key = seedKey(phone.number);
    if (key) {
      return key;
    }
  }
  return `contact:${contactId}`;
}

/** Extracts up to max uppercase Unicode alphabetic initial code points. */
export function initials(name: string | undefined, max: number): string {
  if (!name) {
    return "";
  }

  return name
    .split(UNICODE_WHITESPACE)
    .filter(Boolean)
    .map((word) => [...word][0])
    .filter((character) => UNICODE_ALPHABETIC.test(character))
    .flatMap((character) => [...character.toUpperCase()])
    .slice(0, max)
    .join("");
}
