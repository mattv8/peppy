import { useEffect, useRef, useState } from "react";
import type { GalleryAttachment } from "./grouping";

type MediaResult = { state: "ready"; url: string } | { state: "pending" };

export type InlineMediaProps = {
  attachment: GalleryAttachment;
  requestOriginal(id: string): Promise<MediaResult>;
  checkOriginal(id: string): Promise<MediaResult>;
  releaseMedia?(attachmentId: string, url: string): void;
  retryKey?: unknown;
};

function duration(durationMs?: number): string | undefined {
  if (durationMs === undefined) return undefined;
  return `${Math.floor(durationMs / 60_000)}:${String(Math.floor((durationMs % 60_000) / 1_000)).padStart(2, "0")}`;
}

export function InlineMedia({ attachment, requestOriginal, checkOriginal, releaseMedia, retryKey }: InlineMediaProps) {
  const [url, setUrl] = useState<string>();
  const [loading, setLoading] = useState(false);
  const [pending, setPending] = useState(false);
  const [error, setError] = useState<string>();
  const requestVersion = useRef(0);
  const requested = useRef(false);
  const mediaKind = attachment.mediaType.startsWith("video/") ? "video" : attachment.mediaType.startsWith("audio/") ? "audio" : undefined;
  const load = async () => {
    const version = ++requestVersion.current;
    requested.current = true;
    setLoading(true);
    setError(undefined);
    try {
      const result = await requestOriginal(attachment.id);
      if (version !== requestVersion.current) {
        if (result.state === "ready") releaseMedia?.(attachment.id, result.url);
      } else if (result.state === "ready") {
        setPending(false);
        setUrl(result.url);
      } else {
        setPending(true);
      }
    } catch {
      if (version === requestVersion.current) setError("Could not load media.");
    } finally {
      if (version === requestVersion.current) setLoading(false);
    }
  };
  const check = async () => {
    const version = ++requestVersion.current;
    try {
      const result = await checkOriginal(attachment.id);
      if (version !== requestVersion.current) { if (result.state === "ready") releaseMedia?.(attachment.id, result.url); }
      else if (result.state === "ready") { setPending(false); setUrl(result.url); }
    } catch { if (version === requestVersion.current) setError("Could not load media."); }
  };
  useEffect(() => () => {
    requestVersion.current += 1;
    if (url) releaseMedia?.(attachment.id, url);
  }, [attachment.id, releaseMedia, url]);
  useEffect(() => {
    if (pending && requested.current) void check();
  }, [retryKey]);
  if (!mediaKind) return null;
  if (url) return mediaKind === "video" ? <video data-inline-media controls autoPlay preload="metadata" src={url} aria-label={attachment.name || "Video"} /> : <audio data-inline-media controls autoPlay preload="metadata" src={url} aria-label={attachment.name || "Audio"} />;
  return <div data-inline-media>
    <button type="button" className="media-play-trigger" data-media-play-trigger data-media-kind={mediaKind} data-attachment-id={attachment.id} aria-busy={loading || undefined} aria-label={`Play ${mediaKind}: ${attachment.name || mediaKind}`} onClick={() => void load()}>{loading || pending ? "Downloading…" : `Play ${mediaKind}`}</button>
    {duration(attachment.durationMs) && <span data-media-duration>{duration(attachment.durationMs)}</span>}
    {error && <p role="alert">{error}</p>}
  </div>;
}
