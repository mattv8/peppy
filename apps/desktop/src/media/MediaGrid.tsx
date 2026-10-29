import { useEffect, useLayoutEffect, useMemo, useRef, useState, type ReactNode, type RefCallback } from "react";
import { ChevronLeft, ChevronRight, WifiOff } from "@peppy/desktop-ui";
import { groupMessages, type GalleryAttachment, type GalleryMessage, type MessageGroup } from "./grouping";
import { MediaSyncButton, MediaTile, useReturnedToHoldGeneration, type SyncState } from "./MediaTile";
import { demandTarget, holdNoteText, originalPending, originalView } from "./mediaState";
import { useMediaActions } from "./MediaActionsContext";
import "./media.css";
import "./media-carousel.css";

export type MediaGridProps = {
  messages: GalleryMessage[];
  onOpen(item: { attachment: GalleryAttachment; message: GalleryMessage }): void;
  renderMessageMeta?(message: GalleryMessage): ReactNode;
  messageRef?(message: GalleryMessage): RefCallback<HTMLElement> | undefined;
  renderCaption?(message: GalleryMessage): ReactNode;
  messageTitle?(message: GalleryMessage): string | undefined;
  senderRun?(group: MessageGroup): { label?: ReactNode; avatar?: ReactNode; gutter: boolean; ariaName?: string; start: boolean; end: boolean } | undefined;
};

function Carousel({ group, onOpen, messageTitle }: { group: MessageGroup; onOpen: MediaGridProps["onOpen"]; messageTitle?: MediaGridProps["messageTitle"] }) {
  const carouselRef = useRef<HTMLDivElement>(null);
  const trackRef = useRef<HTMLDivElement>(null);
  const bodyRefs = useRef<Array<HTMLButtonElement | null>>([]);
  const slideRefs = useRef<Array<HTMLDivElement | null>>([]);
  const visibleSlideIndexes = useRef(new Set<number>());
  const [activeIndex, setActiveIndex] = useState(0);
  const [scrollState, setScrollState] = useState({ previous: false, next: false });
  const [groupSyncState, setGroupSyncState] = useState<SyncState>("idle");
  const groupControlFocused = useRef(false);
  const total = group.attachments.length;
  const pendingAttachments = group.attachments.filter(({ attachment }) => originalPending(attachment));
  const requestableAttachments = pendingAttachments.filter(({ attachment }) => originalView(attachment).phase === "held");
  const allHeld = total >= 2 && pendingAttachments.length === total;
  const previousAllHeld = useRef(allHeld);
  const holdNoteId = `media-group-hold-note-${group.id}`;
  const actions = useMediaActions();
  const bodyRefCallbacks = useMemo(() => Array.from({ length: total }, (_, index) => (element: HTMLButtonElement | null) => { bodyRefs.current[index] = element; }), [total]);
  const resetGeneration = useReturnedToHoldGeneration(group.attachments.map(({ attachment }) => ({ id: attachment.id, phase: originalView(attachment).phase })));
  const groupResetKey = `${group.attachments.map(({ attachment }) => attachment.id).join(",")}#${resetGeneration}`;
  useEffect(() => { if (resetGeneration) setGroupSyncState("idle"); }, [resetGeneration]);

  useEffect(() => {
    const track = trackRef.current;
    if (!track) return;
    let frame = 0;
    const updateScrollState = () => {
      cancelAnimationFrame(frame);
      frame = requestAnimationFrame(() => {
        const next = { previous: track.scrollLeft > 1, next: track.scrollLeft + track.clientWidth < track.scrollWidth - 1 };
        setScrollState((current) => current.previous === next.previous && current.next === next.next ? current : next);
      });
    };
    updateScrollState();
    track.addEventListener("scroll", updateScrollState, { passive: true });
    if (typeof ResizeObserver === "undefined") return () => {
      track.removeEventListener("scroll", updateScrollState);
      cancelAnimationFrame(frame);
    };
    const observer = new ResizeObserver(updateScrollState);
    observer.observe(track);
    return () => {
      track.removeEventListener("scroll", updateScrollState);
      observer.disconnect();
      cancelAnimationFrame(frame);
    };
  }, []);

  useEffect(() => {
    const track = trackRef.current;
    if (!track || typeof IntersectionObserver === "undefined") return;
    const observer = new IntersectionObserver((entries) => {
      for (const entry of entries) {
        const index = Number((entry.target as HTMLElement).dataset.slideIndex);
        if (entry.isIntersecting && entry.intersectionRatio >= 0.5) visibleSlideIndexes.current.add(index);
        else visibleSlideIndexes.current.delete(index);
      }
      const firstVisible = Math.min(...visibleSlideIndexes.current);
      if (Number.isFinite(firstVisible)) setActiveIndex(firstVisible);
    }, { root: track, threshold: [0.5] });
    slideRefs.current.forEach((slide) => slide && observer.observe(slide));
    return () => observer.disconnect();
  }, [total]);

  useLayoutEffect(() => {
    if (previousAllHeld.current && !allHeld && groupControlFocused.current) {
      const target = carouselRef.current?.querySelector<HTMLButtonElement>("[data-original-sync-button]") ?? bodyRefs.current.find(Boolean) ?? carouselRef.current;
      target?.focus();
    }
    previousAllHeld.current = allHeld;
    groupControlFocused.current = false;
  }, [allHeld]);

  const moveFocus = (index: number, direction: -1 | 1) => {
    for (let candidate = index + direction; candidate >= 0 && candidate < total; candidate += direction) {
      const target = bodyRefs.current[candidate];
      if (!target) continue;
      target.focus();
      target.scrollIntoView?.({ inline: "nearest", block: "nearest" });
      return true;
    }
    return false;
  };
  const scrollByPage = (direction: 1 | -1) => {
    const track = trackRef.current;
    if (!track) return;
    const reducedMotion = window.matchMedia?.("(prefers-reduced-motion: reduce)").matches;
    track.scrollBy({ left: direction * (track.clientWidth - 56), behavior: reducedMotion ? "auto" : "smooth" });
  };

  const noteState = groupSyncState === "requesting" ? "requesting" : requestableAttachments.length === 0 ? "queued" : groupSyncState;
  const groupStatus = holdNoteText(noteState, Boolean(actions?.canRequestMediaSync), pendingAttachments.find(({ attachment }) => attachment.error)?.attachment.error);
  const groupLabel = requestableAttachments.length ? `Sync ${requestableAttachments.length} images` : "Requested";
  return <><div ref={carouselRef} tabIndex={-1} data-media-carousel data-sync-owner={allHeld ? "group" : undefined} data-sync={groupSyncState} role="region" aria-roledescription="carousel" aria-label={`Images, ${total} total`} data-slide-count={total} data-has-peek={total >= 4 || undefined} data-can-scroll-prev={scrollState.previous || undefined} data-can-scroll-next={scrollState.next || undefined}>
    <div ref={trackRef} className="carousel-track">
      {group.attachments.map(({ attachment, message }, index) => <div key={`${message.id}:${attachment.id}`} ref={(element) => { slideRefs.current[index] = element; }} data-media-slide data-slide-index={index} role="group" aria-roledescription="slide" aria-label={`${index + 1} of ${total}`} title={messageTitle?.(message)}>
        <MediaTile attachment={attachment} message={message} variant="slide" label={`Photo, ${index + 1} of ${total}`} onOpen={() => onOpen({ attachment, message })} suppressHeldControl={allHeld} prefetchIds={[group.attachments[index + 1] ? demandTarget(group.attachments[index + 1].attachment) : undefined]} bodyRef={bodyRefCallbacks[index]} onBodyKeyDown={(event) => {
          if (event.key === "ArrowLeft" && moveFocus(index, -1)) event.preventDefault();
          if (event.key === "ArrowRight" && moveFocus(index, 1)) event.preventDefault();
        }} />
      </div>)}
    </div>
    {allHeld && <MediaSyncButton attachmentIds={(requestableAttachments.length ? requestableAttachments : pendingAttachments).map(({ attachment }) => attachment.id)} label={groupLabel} ariaLabel={`${groupLabel} using mobile data if needed`} retryAriaLabel={`Request again ${pendingAttachments.length} images using mobile data if needed`} describedBy={holdNoteId} initialState={requestableAttachments.length ? undefined : "queued"} resetKey={groupResetKey} className="carousel-sync" dataAttributes={{ "data-group-sync": group.id }} onFocus={() => { groupControlFocused.current = true; }} onBlur={() => { groupControlFocused.current = false; }} onStateChange={setGroupSyncState} />}
    {scrollState.previous && <button type="button" className="carousel-chevron carousel-chevron-previous" aria-label="Previous images" onClick={() => scrollByPage(-1)}><ChevronLeft size={18} aria-hidden="true" /></button>}
    {scrollState.next && <button type="button" className="carousel-chevron carousel-chevron-next" aria-label="Next images" onClick={() => scrollByPage(1)}><ChevronRight size={18} aria-hidden="true" /></button>}
    <span className="carousel-counter" aria-hidden="true">{activeIndex + 1} / {total}</span>
  </div>{allHeld && <p id={holdNoteId} data-group-hold-note aria-live="off"><WifiOff size={12} aria-hidden="true" />{groupStatus}</p>}</>;
}

function Group({ group, onOpen, renderMessageMeta, messageRef, renderCaption, messageTitle, senderRun }: { group: MessageGroup; onOpen: MediaGridProps["onOpen"]; renderMessageMeta?: MediaGridProps["renderMessageMeta"]; messageRef?: MediaGridProps["messageRef"]; renderCaption?: MediaGridProps["renderCaption"]; messageTitle?: MediaGridProps["messageTitle"]; senderRun?: MediaGridProps["senderRun"] }) {
  const finalMessage = group.messages.at(-1);
  const isGrouped = group.messages.length > 1 || group.attachments.length > 1;
  const onlyAttachment = group.attachments[0];
  const sender = senderRun?.(group);
  const article = <article data-grouped={isGrouped || undefined} data-sender={group.messages[0].sender} data-sender-address={group.messages[0].senderAddress} aria-label={sender?.ariaName ? `${group.attachments.length} images from ${sender.ariaName}` : `${group.attachments.length} images`} title={group.attachments.length === 1 && onlyAttachment ? messageTitle?.(onlyAttachment.message) : undefined}>
      {group.attachments.length === 1 && onlyAttachment ? <MediaTile attachment={onlyAttachment.attachment} message={onlyAttachment.message} variant="single" onOpen={() => onOpen(onlyAttachment)} /> : <Carousel group={group} onOpen={onOpen} messageTitle={messageTitle} />}
      {finalMessage?.body && <p data-group-caption>{renderCaption?.(finalMessage) ?? finalMessage.body}</p>}
      {finalMessage?.subject && <p data-group-subject>Subject: {finalMessage.subject}</p>}
    </article>;
  return <div className={`message-row ${group.messages[0].sender}`} data-message-group={group.id} data-sender={group.messages[0].sender} data-sender-gutter={sender?.gutter ? "" : undefined} data-sender-run-start={sender?.start ? "" : undefined} data-sender-run-end={sender?.end ? "" : undefined}>
    {sender?.label}
    {sender ? <div className="message-sender-line" data-sender-line>{sender.avatar ?? (sender.gutter && <span className="message-sender-gutter" aria-hidden="true" />)}{article}</div> : article}
    {group.messages.map((message) => <div key={message.id} ref={messageRef?.(message)} data-message-row={message.id}><div data-message-id={message.id} data-message-meta={message.id}>{renderMessageMeta?.(message)}</div></div>)}
  </div>;
}

export function MediaGrid(props: MediaGridProps) {
  return <section data-media-message-grid>{groupMessages(props.messages).map((group) => <Group key={group.id} group={group} {...props} />)}</section>;
}
