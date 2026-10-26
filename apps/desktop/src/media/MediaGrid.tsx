import { useEffect, useMemo, useRef, useState, type ReactNode, type RefCallback } from "react";
import { ChevronLeft, ChevronRight } from "@peppy/desktop-ui";
import { groupMessages, type GalleryAttachment, type GalleryMessage, type MessageGroup } from "./grouping";
import { MediaTile } from "./MediaTile";
import { demandTarget } from "./mediaState";
import "./media.css";
import "./media-carousel.css";

export type MediaGridProps = {
  messages: GalleryMessage[];
  onOpen(item: { attachment: GalleryAttachment; message: GalleryMessage }): void;
  renderMessageMeta?(message: GalleryMessage): ReactNode;
  messageRef?(message: GalleryMessage): RefCallback<HTMLElement> | undefined;
  renderCaption?(message: GalleryMessage): ReactNode;
};

function Carousel({ group, onOpen }: { group: MessageGroup; onOpen: MediaGridProps["onOpen"] }) {
  const trackRef = useRef<HTMLDivElement>(null);
  const bodyRefs = useRef<Array<HTMLButtonElement | null>>([]);
  const slideRefs = useRef<Array<HTMLDivElement | null>>([]);
  const visibleSlideIndexes = useRef(new Set<number>());
  const [activeIndex, setActiveIndex] = useState(0);
  const [scrollState, setScrollState] = useState({ previous: false, next: false });
  const total = group.attachments.length;
  const bodyRefCallbacks = useMemo(() => Array.from({ length: total }, (_, index) => (element: HTMLButtonElement | null) => { bodyRefs.current[index] = element; }), [total]);

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

  return <div data-media-carousel role="region" aria-roledescription="carousel" aria-label={`Images, ${total} total`} data-slide-count={total} data-has-peek={total >= 4 || undefined} data-can-scroll-prev={scrollState.previous || undefined} data-can-scroll-next={scrollState.next || undefined}>
    <div ref={trackRef} className="carousel-track">
      {group.attachments.map(({ attachment, message }, index) => <div key={`${message.id}:${attachment.id}`} ref={(element) => { slideRefs.current[index] = element; }} data-media-slide data-slide-index={index} role="group" aria-roledescription="slide" aria-label={`${index + 1} of ${total}`}>
        <MediaTile attachment={attachment} message={message} variant="slide" label={`Photo, ${index + 1} of ${total}`} onOpen={() => onOpen({ attachment, message })} prefetchIds={[group.attachments[index + 1] ? demandTarget(group.attachments[index + 1].attachment) : undefined]} bodyRef={bodyRefCallbacks[index]} onBodyKeyDown={(event) => {
          if (event.key === "ArrowLeft" && moveFocus(index, -1)) event.preventDefault();
          if (event.key === "ArrowRight" && moveFocus(index, 1)) event.preventDefault();
        }} />
      </div>)}
    </div>
    {scrollState.previous && <button type="button" className="carousel-chevron carousel-chevron-previous" aria-label="Previous images" onClick={() => scrollByPage(-1)}><ChevronLeft size={18} aria-hidden="true" /></button>}
    {scrollState.next && <button type="button" className="carousel-chevron carousel-chevron-next" aria-label="Next images" onClick={() => scrollByPage(1)}><ChevronRight size={18} aria-hidden="true" /></button>}
    <span className="carousel-counter" aria-hidden="true">{activeIndex + 1} / {total}</span>
  </div>;
}

function Group({ group, onOpen, renderMessageMeta, messageRef, renderCaption }: { group: MessageGroup; onOpen: MediaGridProps["onOpen"]; renderMessageMeta?: MediaGridProps["renderMessageMeta"]; messageRef?: MediaGridProps["messageRef"]; renderCaption?: MediaGridProps["renderCaption"] }) {
  const finalMessage = group.messages.at(-1);
  const isGrouped = group.messages.length > 1 || group.attachments.length > 1;
  const onlyAttachment = group.attachments[0];
  return <div className={`message-row ${group.messages[0].sender}`} data-message-group={group.id} data-sender={group.messages[0].sender}>
    <article data-grouped={isGrouped || undefined} data-sender={group.messages[0].sender} data-sender-address={group.messages[0].senderAddress} aria-label={`${group.attachments.length} images`}>
      {group.attachments.length === 1 && onlyAttachment ? <MediaTile attachment={onlyAttachment.attachment} message={onlyAttachment.message} variant="single" onOpen={() => onOpen(onlyAttachment)} /> : <Carousel group={group} onOpen={onOpen} />}
      {finalMessage?.body && <p data-group-caption>{renderCaption?.(finalMessage) ?? finalMessage.body}</p>}
      {finalMessage?.subject && <p data-group-subject>Subject: {finalMessage.subject}</p>}
    </article>
    {group.messages.map((message) => <div key={message.id} ref={messageRef?.(message)} data-message-row={message.id}><div data-message-id={message.id} data-message-meta={message.id}>{renderMessageMeta?.(message)}</div></div>)}
  </div>;
}

export function MediaGrid(props: MediaGridProps) {
  return <section data-media-message-grid>{groupMessages(props.messages).map((group) => <Group key={group.id} group={group} {...props} />)}</section>;
}
