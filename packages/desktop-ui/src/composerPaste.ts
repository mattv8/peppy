export type ComposerPasteRejection = {
  name: string;
  reason: "type" | "size" | "limit";
};

export type ComposerPaste = {
  images: File[];
  text: string | null;
  rejected: ComposerPasteRejection[];
};

export const MAX_COMPOSER_ATTACHMENTS = 10;
export const MAX_COMPOSER_ATTACHMENT_BYTES = 32 * 1024 * 1024;
export const MAX_COMPOSER_ATTACHMENT_SIZE_LABEL = "32 MiB";

const ACCEPTED_IMAGE_TYPES = new Set(["image/png", "image/jpeg", "image/gif", "image/webp"]);

function isSingleUrl(value: string): boolean {
  const trimmed = value.trim();
  if (!trimmed || /\s/.test(trimmed)) return false;
  try {
    const url = new URL(trimmed);
    return url.protocol === "http:" || url.protocol === "https:";
  } catch {
    return false;
  }
}

export function extractComposerPaste(data: DataTransfer, remainingSlots: number): ComposerPaste {
  const rejected: ComposerPasteRejection[] = [];
  const images: File[] = [];
  const files = Array.from(data.files);

  for (const file of files) {
    if (!file.type.startsWith("image/")) continue;
    if (!ACCEPTED_IMAGE_TYPES.has(file.type)) {
      rejected.push({ name: file.name, reason: "type" });
    } else if (file.size > MAX_COMPOSER_ATTACHMENT_BYTES) {
      rejected.push({ name: file.name, reason: "size" });
    } else if (images.length >= Math.max(0, remainingSlots)) {
      rejected.push({ name: file.name, reason: "limit" });
    } else {
      images.push(file);
    }
  }

  const text = data.getData("text/plain");
  const imageNames = new Set(files.map((file) => file.name));
  const acceptedText = text && !imageNames.has(text) && !isSingleUrl(text) ? text : null;

  return { images, text: acceptedText, rejected };
}
