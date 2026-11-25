import { useEffect, useRef, useState, type KeyboardEvent, type MouseEvent, type SyntheticEvent } from "react";
import { attachmentStorageRemovalText } from "@peppy/desktop-ui";
import type { GalleryAttachment } from "./grouping";
import { peppyCopy } from "../generated/peppyCopy";
import "./inline-media.css";

type MediaResult = { state: "ready"; url: string } | { state: "pending" };
const RATIO_RECONCILIATION_THRESHOLD = 0.01;
const CONTROLS_IDLE_MS = 2000;

export type InlineMediaProps = {
  attachment: GalleryAttachment;
  requestOriginal(id: string): Promise<MediaResult>;
  checkOriginal(id: string): Promise<MediaResult>;
  releaseMedia?(attachmentId: string, url: string): void;
  retryKey?: unknown;
};

function duration(durationMs?: string): string | undefined {
  const parsedDuration = Number(durationMs);
  if (!durationMs || !Number.isFinite(parsedDuration) || parsedDuration < 0) return undefined;
  return `${Math.floor(parsedDuration / 60_000)}:${String(Math.floor((parsedDuration % 60_000) / 1_000)).padStart(2, "0")}`;
}

export function InlineMedia({ attachment, requestOriginal, checkOriginal, releaseMedia, retryKey }: InlineMediaProps) {
  const [url, setUrl] = useState<string>();
  const [loading, setLoading] = useState(false);
  const [pending, setPending] = useState(false);
  const [error, setError] = useState<string>();
  const requestVersion = useRef(0);
  const requested = useRef(false);
  const videoRef = useRef<HTMLVideoElement>(null);
  const focusVideoAfterPlay = useRef(false);
  const [controlsVisible, setControlsVisible] = useState(true);
  const controlsTimer = useRef<ReturnType<typeof setTimeout> | undefined>(undefined);
  const playing = useRef(false);
  const seeking = useRef(false);
  const keyboardFocus = useRef(false);
  const keyboardInput = useRef(false);
  const touchOnly = useRef(false);
  const mediaKind = attachment.mediaType.startsWith("video/") ? "video" : attachment.mediaType.startsWith("audio/") ? "audio" : undefined;
  const mediaDuration = duration(attachment.mediaMeta?.durationMs);
  const removedRemoteOnly = attachment.remoteState === "removed_by_storage_policy" && !attachment.localAvailable;
  const removedWithLocalCopy = attachment.remoteState === "removed_by_storage_policy" && attachment.localAvailable;
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
        focusVideoAfterPlay.current = false;
        setPending(true);
      }
    } catch {
      if (version === requestVersion.current) {
        focusVideoAfterPlay.current = false;
        setError("Could not load media.");
      }
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
  useEffect(() => {
    if (!url || !focusVideoAfterPlay.current) return;
    videoRef.current?.focus();
    focusVideoAfterPlay.current = false;
  }, [url]);
  useEffect(() => {
    const media = window.matchMedia?.("(hover: none)");
    setControlsVisible(true);
    const updateHover = () => {
      touchOnly.current = media?.matches ?? false;
      if (touchOnly.current) setControlsVisible(true);
    };
    // Pointer focus does not pin the controls; keyboard focus does.
    const onKeyDown = () => { keyboardInput.current = true; };
    const onPointerDown = () => { keyboardInput.current = false; };
    updateHover();
    media?.addEventListener("change", updateHover);
    window.addEventListener("keydown", onKeyDown, true);
    window.addEventListener("pointerdown", onPointerDown, true);
    return () => {
      clearTimeout(controlsTimer.current);
      playing.current = false;
      seeking.current = false;
      keyboardFocus.current = false;
      media?.removeEventListener("change", updateHover);
      window.removeEventListener("keydown", onKeyDown, true);
      window.removeEventListener("pointerdown", onPointerDown, true);
    };
  }, [url]);
  const showControls = () => {
    clearTimeout(controlsTimer.current);
    setControlsVisible(true);
    if (playing.current && !seeking.current && !keyboardFocus.current && !touchOnly.current) {
      controlsTimer.current = setTimeout(() => setControlsVisible(false), CONTROLS_IDLE_MS);
    }
  };
  const hideControls = () => {
    clearTimeout(controlsTimer.current);
    if (playing.current && !seeking.current && !keyboardFocus.current && !touchOnly.current) setControlsVisible(false);
  };
  const handleVideoKeyDown = (event: KeyboardEvent<HTMLVideoElement>) => {
    if (event.key !== " " && event.key.toLowerCase() !== "k") return;
    event.preventDefault();
    if (event.currentTarget.paused) void event.currentTarget.play().catch(() => {});
    else event.currentTarget.pause();
  };
  const handlePlay = (event: MouseEvent<HTMLButtonElement>) => {
    focusVideoAfterPlay.current = document.activeElement === event.currentTarget;
    void load();
  };
  const reconcileVideoRatio = (event: SyntheticEvent<HTMLVideoElement>) => {
    const video = event.currentTarget;
    if (video.videoWidth <= 0 || video.videoHeight <= 0) return;
    const measuredRatio = video.videoWidth / video.videoHeight;
    const [width, height] = getComputedStyle(video).getPropertyValue("--video-ar").split("/").map(Number);
    const appliedRatio = width > 0 && height > 0 ? width / height : 16 / 9;
    if (Math.abs(measuredRatio / appliedRatio - 1) - RATIO_RECONCILIATION_THRESHOLD > Number.EPSILON) video.style.setProperty("--video-ar", `${video.videoWidth} / ${video.videoHeight}`);
  };
  if (!mediaKind) return null;
  if (removedRemoteOnly) return <div data-inline-media data-media-kind={mediaKind} data-attachment-id={attachment.id} data-attachment-storage-removed role="status">{attachmentStorageRemovalText(false, peppyCopy).body}</div>;
  if (url) return mediaKind === "video" ? <video ref={videoRef} data-inline-media data-media-kind="video" data-attachment-id={attachment.id} data-controls-idle={!controlsVisible || undefined} controls={controlsVisible} autoPlay preload="metadata" src={url} aria-label={attachment.name || "Video"} tabIndex={0} onLoadedMetadata={reconcileVideoRatio}
    onPlay={() => { playing.current = true; showControls(); }}
    onPause={() => { playing.current = false; showControls(); }}
    onEnded={() => { playing.current = false; showControls(); }}
    onPointerMove={showControls} onPointerEnter={showControls} onPointerLeave={hideControls}
    onClick={() => { if (!controlsVisible) showControls(); }}
    onFocus={() => { keyboardFocus.current = keyboardInput.current; showControls(); }}
    onBlur={() => { keyboardFocus.current = false; showControls(); }}
    onKeyDown={event => { keyboardFocus.current = true; showControls(); handleVideoKeyDown(event); }}
    onSeeking={() => { seeking.current = true; showControls(); }}
    onSeeked={() => { seeking.current = false; showControls(); }}
    onVolumeChange={showControls} /> : <audio data-inline-media data-media-kind="audio" controls autoPlay preload="metadata" src={url} aria-label={attachment.name || "Audio"} />;
  const isBusy = loading || pending;
  const buttonLabel = isBusy ? `Downloading ${mediaKind}: ${attachment.name || mediaKind}` : `Play ${mediaKind}: ${attachment.name || mediaKind}`;
  return <div data-inline-media data-media-kind={mediaKind} data-attachment-id={attachment.id}>
    <button type="button" className={mediaKind === "video" ? "media-play-trigger" : "secondary-button"} data-media-play-trigger data-media-kind={mediaKind} data-attachment-id={attachment.id} aria-busy={isBusy || undefined} aria-label={buttonLabel} onClick={handlePlay}>{isBusy ? "Downloading…" : `Play ${mediaKind}`}</button>
    {mediaDuration && <span data-media-duration>{mediaDuration}</span>}
    {error && <p role="alert">{error}</p>}
    {removedWithLocalCopy && <p data-attachment-storage-local-copy role="status">{attachmentStorageRemovalText(true, peppyCopy).body}</p>}
  </div>;
}
