const OVERHEAD_BYTES = 16 * 1024;
const MIN_IMAGE_BUDGET_BYTES = 90 * 1024;
const MAX_MMS_ATTACHMENTS = 10;
const RESIZABLE_IMAGE_TYPES = new Set(["image/jpeg", "image/png", "image/webp", "image/heic", "image/heif"]);

type MmsPlanImage = {
  id: string;
  bytes: number;
  mediaType: string;
};

type MmsPlanInput = {
  limitBytes: number;
  gatewayVersion: number | undefined;
  text: string;
  images: MmsPlanImage[];
};

export type MmsPlan =
  | { kind: "single"; parts: 1 }
  | { kind: "split"; parts: number; caption: boolean; groups: string[][] }
  | { kind: "error"; parts: 0; imageId?: string };

function textBytes(text: string): number {
  return new TextEncoder().encode(text).length;
}

function balancedGroups(images: MmsPlanImage[], groupCount: number): string[][] {
  const smallerGroupSize = Math.floor(images.length / groupCount);
  const largerGroupCount = images.length % groupCount;
  let offset = 0;
  return Array.from({ length: groupCount }, (_, index) => {
    const size = smallerGroupSize + (index < largerGroupCount ? 1 : 0);
    const group = images.slice(offset, offset + size).map(({ id }) => id);
    offset += size;
    return group;
  });
}

export function planMms({ limitBytes, gatewayVersion, text, images }: MmsPlanInput): MmsPlan {
  const totalBytes = textBytes(text) + images.reduce((total, image) => total + image.bytes, 0);
  if (totalBytes <= limitBytes) return { kind: "single", parts: 1 };
  if ((gatewayVersion ?? 0) < 4) return { kind: "error", parts: 0 };

  const nonResizableImage = images.find((image) =>
    !RESIZABLE_IMAGE_TYPES.has(image.mediaType.toLowerCase()) && image.bytes > MIN_IMAGE_BUDGET_BYTES,
  );
  if (nonResizableImage) return { kind: "error", parts: 0, imageId: nonResizableImage.id };

  const imagesPerMessage = Math.max(
    1,
    Math.min(MAX_MMS_ATTACHMENTS, Math.floor((limitBytes - OVERHEAD_BYTES) / MIN_IMAGE_BUDGET_BYTES)),
  );
  if (images.length <= imagesPerMessage) return { kind: "single", parts: 1 };

  const groups = balancedGroups(images, Math.ceil(images.length / imagesPerMessage));
  const caption = text.trim().length > 0;
  return { kind: "split", parts: groups.length + Number(caption), caption, groups };
}
