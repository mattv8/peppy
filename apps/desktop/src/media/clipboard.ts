import type { MediaResult } from "./MediaActionsContext";

export class MediaPendingError extends Error {
  constructor() {
    super("Image is still downloading from your phone");
    this.name = "MediaPendingError";
  }
}

export function clipboardImageSupported(): boolean {
  return typeof ClipboardItem !== "undefined" && typeof navigator !== "undefined" && typeof navigator.clipboard?.write === "function";
}

export async function writeImageToClipboard(png: Promise<Blob>): Promise<void> {
  if (!clipboardImageSupported()) {
    void png.catch(() => undefined);
    throw new Error("Image clipboard is unavailable");
  }
  let write: Promise<void>;
  try {
    write = navigator.clipboard.write([new ClipboardItem({ "image/png": png })]);
  } catch (error) {
    try {
      await png;
    } catch (pngError) {
      throw pngError;
    }
    throw error;
  }
  try {
    await write;
  } catch (error) {
    try {
      await png;
    } catch (pngError) {
      throw pngError;
    }
    throw error;
  }
  await png;
}

function canvasPng(bitmap: ImageBitmap): Promise<Blob> {
  const canvas = document.createElement("canvas");
  canvas.width = bitmap.width;
  canvas.height = bitmap.height;
  const context = canvas.getContext("2d");
  if (!context) return Promise.reject(new Error("Canvas is unavailable"));
  context.drawImage(bitmap, 0, 0);
  return new Promise((resolve, reject) => {
    canvas.toBlob((blob) => blob ? resolve(blob) : reject(new Error("Could not encode image")), "image/png");
  });
}

export async function originalAsPng(load: () => Promise<MediaResult>, release: (url: string) => void): Promise<Blob> {
  const media = await load();
  if (media.state === "pending") throw new MediaPendingError();
  try {
    const response = await fetch(media.url);
    if (!response.ok) throw new Error(`Could not load image (${response.status})`);
    const blob = await response.blob();
    if (blob.type === "image/png") return blob;
    const bitmap = await createImageBitmap(blob);
    try {
      return await canvasPng(bitmap);
    } finally {
      bitmap.close();
    }
  } finally {
    release(media.url);
  }
}
