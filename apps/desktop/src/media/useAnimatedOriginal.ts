import { useEffect, useRef, useState, type RefObject } from "react";
import type { GalleryAttachment } from "./grouping";
import { useMediaActions } from "./MediaActionsContext";
import { originalPending } from "./mediaState";
import { useReducedMotion } from "./useReducedMotion";

const MAX_ANIMATED = 4;
const MAX_BYTES = 8 * 1024 * 1024;
let slots = 0;
const subscribers = new Set<() => void>();

function freeSlot() {
  slots--;
  subscribers.forEach((notify) => notify());
}

export function useAnimatedOriginal(rootRef: RefObject<HTMLDivElement | null>, attachment: GalleryAttachment, enabled: boolean): string | undefined {
  const actions = useMediaActions();
  const reduced = useReducedMotion();
  const [visible, setVisible] = useState(typeof IntersectionObserver === "undefined");
  const [loaded, setLoaded] = useState<{ key: string; url: string }>();
  const [slotVersion, setSlotVersion] = useState(0);
  const failed = useRef<string | undefined>(undefined);
  const waiting = useRef(false);
  const key = `${attachment.id}\0${attachment.state}`;
  const previousKey = useRef(key);
  if (previousKey.current !== key) {
    previousKey.current = key;
    failed.current = undefined;
  }
  const eligible = enabled && attachment.mediaType === "image/gif" && !reduced && !originalPending(attachment)
    && attachment.byteSize <= MAX_BYTES && attachment.state === "ready"
    && !(attachment.remoteState === "removed_by_storage_policy" && !attachment.localAvailable);
  const loadOriginal = actions?.loadOriginal;
  const releaseMedia = actions?.releaseMedia;

  useEffect(() => {
    const root = rootRef.current;
    if (!eligible || !root || typeof IntersectionObserver === "undefined") return;
    setVisible(false);
    const observer = new IntersectionObserver((entries) => setVisible(entries.some((entry) => entry.isIntersecting)), { rootMargin: "200px" });
    observer.observe(root);
    return () => observer.disconnect();
  }, [rootRef, attachment.id, eligible]);

  useEffect(() => {
    const notify = () => { if (waiting.current) setSlotVersion((version) => version + 1); };
    subscribers.add(notify);
    return () => { subscribers.delete(notify); };
  }, []);

  useEffect(() => {
    waiting.current = false;
    if (!eligible || !visible || !loadOriginal || failed.current === key) return;
    if (slots >= MAX_ANIMATED) {
      waiting.current = true;
      return () => { waiting.current = false; };
    }
    slots++;
    let active = true;
    let heldUrl: string | undefined;
    let ownsSlot = true;
    const releaseSlot = () => {
      if (!ownsSlot) return;
      ownsSlot = false;
      freeSlot();
    };
    void Promise.resolve().then(() => loadOriginal(attachment.id)).then((result) => {
      if (result.state === "pending") {
        if (active) failed.current = key;
        releaseSlot();
      } else if (!active) {
        releaseMedia?.(attachment.id, result.url);
        releaseSlot();
      } else {
        heldUrl = result.url;
        setLoaded({ key, url: result.url });
      }
    }, () => {
      if (active) failed.current = key;
      releaseSlot();
    });
    return () => {
      active = false;
      setLoaded(undefined);
      if (heldUrl) {
        releaseMedia?.(attachment.id, heldUrl);
        releaseSlot();
      }
    };
  }, [attachment.id, eligible, key, loadOriginal, releaseMedia, visible, slotVersion]);

  return eligible && visible && loaded?.key === key ? loaded.url : undefined;
}
