const INLINE_MEDIA_TYPES = new Set([
  "image/gif", "image/jpeg", "image/png", "image/webp", "video/3gpp", "video/mp4", "audio/aac", "audio/amr", "audio/mp4", "audio/mpeg", "audio/ogg", "audio/opus", "audio/wav", "audio/x-m4a",
]);
const MAX_FILENAME_CHARACTERS = 128;

function normalizedMediaType(mediaType: string): string {
  return mediaType.trim().toLowerCase();
}

/** Matches the desktop allowlist: active document types never receive a rendering URL. */
export function canInlineMedia(mediaType: string): boolean {
  return INLINE_MEDIA_TYPES.has(normalizedMediaType(mediaType));
}

export function inlineMediaType(mediaType: string): string | undefined {
  const normalized = normalizedMediaType(mediaType);
  return INLINE_MEDIA_TYPES.has(normalized) ? normalized : undefined;
}

/** Removes only path, control, and bidi-spoofing characters from a browser download suggestion. */
export function safeSuggestedFilename(name: string): string {
  const characters = Array.from(name)
    .filter(character => !/\p{Cc}/u.test(character) && !"/\\".includes(character) && !/[\u200e\u200f\u202a-\u202e\u2066-\u2069]/u.test(character))
  if (!characters.length) return "attachment";
  if (characters.length <= MAX_FILENAME_CHARACTERS) return characters.join("");
  const extensionIndex = characters.lastIndexOf(".");
  const extension = extensionIndex > 0 ? characters.slice(extensionIndex) : [];
  const stem = characters.slice(0, extensionIndex > 0 ? extensionIndex : characters.length);
  return `${stem.slice(0, Math.max(1, MAX_FILENAME_CHARACTERS - extension.length)).join("")}${extension.join("")}`;
}
