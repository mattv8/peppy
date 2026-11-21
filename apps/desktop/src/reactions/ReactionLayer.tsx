import { UPDATE_PAUSED_DESCRIPTION } from "@peppy/desktop-ui";
import { createPortal } from "react-dom";
import { useCallback, useEffect, useLayoutEffect, useRef, useState, type KeyboardEvent, type RefObject } from "react";
import { Clock, Info, MessageSquare, SmilePlus, TriangleAlert, WifiOff, detectPlatform } from "@peppy/desktop-ui";
import type { MessageView, ReactionRoute, ReactionSubmitView, ReactionSupport } from "@peppy/desktop-ui/bridge-contracts";
import { ALREADY_REACTION_COPY, pickAnnouncement, REFUSAL_COPY, UNREACHABLE_COPY, withFirstEmojiNote } from "./announcements";
import { QUICK_REACTIONS } from "./emoji";
import { ReactionCapture } from "./ReactionCapture";
import { barMetrics, placeReactionBar, type ReactionPlacement } from "./placement";
import type { ReactionTargetControls } from "./useReactionTarget";

export type BarNotice = "offline" | "update-needed" | "unconfirmed" | "busy";
export type BarView = { route: ReactionRoute; notice?: BarNotice };

export const NOTICE_COPY: Record<BarNotice, string> = {
  offline: "Phone is offline. Check it and try again.",
  "update-needed": "Update Peppy on your phone to react in this chat.",
  unconfirmed: "Dismiss your earlier reaction first.",
  busy: "Sending your reaction…",
};

const NOTICE_ICON: Record<BarNotice, typeof Clock> = { offline: WifiOff, "update-needed": Info, unconfirmed: TriangleAlert, busy: Clock };

/** Which bar a message gets: nothing, a route with buttons, or a route with a pill instead of buttons. */
export function barViewFor(message: MessageView, reactionSupport: ReactionSupport | undefined, routeOffline: boolean): BarView | undefined {
  if (message.reactable) {
    const route = message.reactable;
    if (routeOffline) return { route, notice: "offline" };
    if (message.ownReactionRequest?.state === "unknown") return { route, notice: "unconfirmed" };
    if (route === "native" && message.ownReactionRequest?.state === "sending") return { route, notice: "busy" };
    return { route };
  }
  if (reactionSupport === "needs_phone_update" && message.transport === "rcs") return { route: "native", notice: "update-needed" };
  return undefined;
}

/** Only a message that can take a reaction, or that is on a route that needs a phone update, shows the bar. */
export function isReactionEligible(message: MessageView, reactionSupport: ReactionSupport | undefined): boolean {
  return Boolean(message.reactable) || (reactionSupport === "needs_phone_update" && message.transport === "rcs");
}

export function nativeSelection(message: MessageView): string | undefined {
  return message.reactions?.find((reaction) => reaction.actors.some((actor) => actor.self))?.emoji;
}

function isCoarsePointer(): boolean {
  return typeof window !== "undefined" && typeof window.matchMedia === "function" && window.matchMedia("(pointer: coarse)").matches;
}

/** The first tabbable element after `anchor` in document order, outside it. */
function nextFocusableAfter(anchor: HTMLElement | null): HTMLElement | null {
  if (!anchor) return null;
  const candidates = document.querySelectorAll<HTMLElement>("button:not([disabled]), input:not([disabled]), textarea:not([disabled]), select:not([disabled]), a[href], [tabindex]:not([tabindex='-1'])");
  return Array.from(candidates).find((node) => !anchor.contains(node) && Boolean(anchor.compareDocumentPosition(node) & Node.DOCUMENT_POSITION_FOLLOWING)) ?? null;
}

/** One announcement, as the owner holds it. A new object with a new `seq` is a new announcement. */
export type Announcement = { seq: number; polite: string; alert: string };
export const NO_ANNOUNCEMENT: Announcement = { seq: 0, polite: "", alert: "" };

/**
 * The two live regions outside `#message-list`. Both always exist, so nothing is inserted with content. Each change
 * clears the text and sets it on the next frame, so a repeated line is announced again. Failures use assertive, not role=alert.
 */
export function ReactionAnnouncer({ announcement }: { announcement: Announcement }) {
  const [polite, setPolite] = useState("");
  const [alert, setAlert] = useState("");
  useEffect(() => {
    setPolite("");
    setAlert("");
    const frame = requestAnimationFrame(() => {
      setPolite(announcement.polite);
      setAlert(announcement.alert);
    });
    return () => cancelAnimationFrame(frame);
  }, [announcement]);
  return <>
    <span id="reaction-announcer" className="sr-only" role="status" aria-live="polite" aria-atomic="true">{polite}</span>
    <span id="reaction-alert" className="sr-only" aria-live="assertive" aria-atomic="true">{alert}</span>
  </>;
}

/** "React to photo 2 of 4 from Alex" for a slide of a run; "React to photo from Alex" for a single photo. */
function photoLabel(target: HTMLElement, name: string): string {
  if (!target.hasAttribute("data-media-slide")) return `React to photo from ${name}`;
  const slides = Array.from(target.parentElement?.children ?? []).filter((node) => node.hasAttribute("data-media-slide"));
  const index = slides.indexOf(target);
  return index < 0 || slides.length < 2 ? `React to photo from ${name}` : `React to photo ${index + 1} of ${slides.length} from ${name}`;
}

function toolbarLabel(route: ReactionRoute, message: MessageView, target: HTMLElement, nameOf: (message: MessageView) => string): string {
  if (route === "text") return "React with a text message";
  if (target.dataset.reactionMessage) return photoLabel(target, nameOf(message));
  return message.sender === "self" ? "React to your message" : `React to message from ${nameOf(message)}`;
}

export type MessageReactionLayerProps = {
  writesPaused?: boolean;
  reaction: ReactionTargetControls;
  barRef: RefObject<HTMLDivElement | null>;
  messages: ReadonlyMap<string, MessageView>;
  reactionSupport?: ReactionSupport;
  /** The route's gateway is not online. */
  routeOffline: boolean;
  /** Accessible name of the sender, used in toolbar labels. */
  nameOf(message: MessageView): string;
  /** Writes to the announcers that the owner renders outside the log. */
  announce(text: string, alert?: boolean): void;
  openPicker?: () => Promise<void>;
  onSubmit(message: MessageView, emoji: string | null): Promise<ReactionSubmitView | undefined>;
};

/**
 * The single reaction bar, portaled to the body and positioned from the target bubble. It is outside `#message-list`,
 * so opening it never changes row layout or fires log announcements.
 */
export function MessageReactionLayer({ reaction, barRef, messages, reactionSupport, routeOffline, writesPaused = false, nameOf, announce, openPicker, onSubmit }: MessageReactionLayerProps) {
  const target = reaction.target;
  const message = target ? messages.get(target.messageId) : undefined;
  const [mode, setMode] = useState<"quick" | "capture">("quick");
  const [placement, setPlacement] = useState<ReactionPlacement | null>(null);
  const [quickCount, setQuickCount] = useState<number>(QUICK_REACTIONS.length);
  const [inFlight, setInFlight] = useState<ReadonlySet<string>>(new Set());
  // Ref, not state: a second pick must see the first one even before React re-renders.
  const inFlightRef = useRef(new Set<string>());
  const [rovingIndex, setRovingIndex] = useState<number>();
  const returnFocus = useRef<HTMLElement | null>(null);
  const focusMoreAfterBack = useRef(false);
  const focusedKeyboardOpen = useRef("");
  const baseView = message ? barViewFor(message, reactionSupport, routeOffline) : undefined;
  // A send from this layer shows busy until the call returns, on either route, before the snapshot catches up.
  const view = baseView && !baseView.notice && message && inFlight.has(message.id)
    ? { route: baseView.route, notice: "busy" as const }
    : baseView;
  const route = view?.route ?? "native";
  const notice = view?.notice;
  const coarse = isCoarsePointer();
  const selected = message && route === "native" ? nativeSelection(message) : undefined;
  const hidden = !target || !message || !view || !placement || reaction.selecting;
  const targetKey = target ? `${target.messageId}:${target.origin}` : "";
  const quick = QUICK_REACTIONS.slice(0, quickCount);
  const selectedIndex = selected ? quick.findIndex(({ emoji }) => emoji === selected) : -1;
  // The toolbar is one tab stop: the selected (or first) button, or the one the arrow keys last reached.
  const tabStop = rovingIndex ?? (selectedIndex >= 0 ? selectedIndex : 0);

  useLayoutEffect(() => {
    setMode("quick");
    setPlacement(null);
    setRovingIndex(undefined);
    returnFocus.current = document.activeElement instanceof HTMLElement ? document.activeElement : null;
  }, [targetKey]);

  const measure = useCallback(() => {
    if (!target || !message) {
      setPlacement(null);
      return;
    }
    const list = target.article.closest<HTMLElement>("#message-list");
    if (!list) return setPlacement(null);
    const anchor = target.article.closest<HTMLElement>("[data-media-message-content]") ?? target.article;
    const row = target.article.closest<HTMLElement>(".message-row");
    const labelNode = message.sender === "other" ? row?.querySelector<HTMLElement>(".message-sender-label") : null;
    const listRect = list.getBoundingClientRect();
    const metrics = barMetrics({ route, coarse, listWidth: listRect.width, capture: mode === "capture" });
    const visual = window.visualViewport;
    const viewport = visual ? { top: visual.offsetTop, bottom: visual.offsetTop + visual.height } : { top: 0, bottom: window.innerHeight };
    setQuickCount(metrics.quick || QUICK_REACTIONS.length);
    setPlacement(placeReactionBar({
      anchor: anchor.getBoundingClientRect(),
      outgoing: message.sender === "self",
      list: listRect,
      composerTop: document.getElementById("composer-area")?.getBoundingClientRect().top ?? window.innerHeight,
      viewport,
      label: labelNode?.getBoundingClientRect(),
      chipsBottom: row?.querySelector<HTMLElement>(".reaction-badges:not(.reaction-badges-overlay)")?.getBoundingClientRect().bottom,
      width: metrics.width,
      height: metrics.height,
      touch: coarse,
    }));
  }, [coarse, message, mode, route, target]);

  useLayoutEffect(() => { measure(); }, [measure]);

  useEffect(() => {
    if (!target) return undefined;
    const list = target.article.closest<HTMLElement>("#message-list");
    const content = document.getElementById("message-list-content");
    let frame = 0;
    const schedule = () => {
      cancelAnimationFrame(frame);
      frame = requestAnimationFrame(measure);
    };
    list?.addEventListener("scroll", schedule, { passive: true });
    const observer = typeof ResizeObserver === "undefined" ? undefined : new ResizeObserver(schedule);
    if (list) observer?.observe(list);
    if (content) observer?.observe(content);
    const visual = window.visualViewport;
    visual?.addEventListener("resize", schedule);
    visual?.addEventListener("scroll", schedule);
    window.addEventListener("resize", schedule);
    return () => {
      cancelAnimationFrame(frame);
      list?.removeEventListener("scroll", schedule);
      observer?.disconnect();
      visual?.removeEventListener("resize", schedule);
      visual?.removeEventListener("scroll", schedule);
      window.removeEventListener("resize", schedule);
    };
  }, [measure, target]);

  // The ring marks the slide under the bar, and only in a run of more than one photo.
  useEffect(() => {
    const article = target?.article;
    if (!article || !article.hasAttribute("data-media-slide")) return undefined;
    const slides = article.parentElement?.querySelectorAll("[data-media-slide]").length ?? 0;
    if (slides < 2) return undefined;
    article.setAttribute("data-reaction-target", "");
    return () => article.removeAttribute("data-reaction-target");
  }, [target]);

  // Keyboard origin opens once per target: focus the bar, or announce the pill and keep focus on the message.
  const keyboardOpen = target?.origin === "keyboard" && !hidden ? targetKey : "";
  useLayoutEffect(() => {
    if (!keyboardOpen) {
      focusedKeyboardOpen.current = "";
      return;
    }
    if (focusedKeyboardOpen.current === keyboardOpen) return;
    focusedKeyboardOpen.current = keyboardOpen;
    if (notice) {
      announce(NOTICE_COPY[notice]);
      return;
    }
    barRef.current?.querySelectorAll<HTMLElement>("button[data-reaction-quick]")[tabStop]?.focus();
  }, [announce, barRef, keyboardOpen, notice, tabStop]);

  useEffect(() => {
    if (mode !== "quick" || !focusMoreAfterBack.current) return;
    focusMoreAfterBack.current = false;
    barRef.current?.querySelector<HTMLElement>("[data-reaction-more]")?.focus();
  }, [barRef, mode]);

  const close = (restoreFocus: boolean) => {
    const focused = document.activeElement;
    const insideBar = focused instanceof Node && Boolean(barRef.current?.contains(focused));
    const origin = target?.origin;
    const article = target?.article;
    reaction.close();
    if (!restoreFocus || !insideBar) return;
    const destination = origin === "keyboard" || origin === "focus" ? article : returnFocus.current;
    const fallback = document.getElementById("message-list");
    const next = destination?.isConnected ? destination : fallback;
    if (next) reaction.restoreFocus(next);
  };

  /**
   * One submit at a time per message, on both routes. A reopened bar during an unresolved submit shows busy and refuses
   * a second call. A rejected call is announced once and never retried.
   */
  const submit = (subject: MessageView, emoji: string | null, text: string) => {
    if (writesPaused || inFlightRef.current.has(subject.id)) return;
    close(true);
    announce(text);
    inFlightRef.current.add(subject.id);
    setInFlight(new Set(inFlightRef.current));
    void onSubmit(subject, emoji)
      .then((result) => {
        if (result && !result.accepted && result.reason) announce(REFUSAL_COPY[result.reason], true);
      })
      .catch(() => announce(UNREACHABLE_COPY, true))
      .finally(() => {
        inFlightRef.current.delete(subject.id);
        setInFlight(new Set(inFlightRef.current));
      });
  };

  /** A pick from the quick row or the capture field. `multiple` is set when capture dropped extra emoji. */
  const choose = (emoji: string, fromCapture: boolean, multiple = false) => {
    if (writesPaused || !message || !view) return;
    if (route === "native" && emoji === selected && fromCapture) {
      close(true);
      announce(ALREADY_REACTION_COPY);
      return;
    }
    const removing = route === "native" && emoji === selected;
    const line = pickAnnouncement({ route, emoji, selected, removing });
    submit(message, removing ? null : emoji, withFirstEmojiNote(line, multiple));
  };

  /**
   * Tab leaves to the composer, Shift+Tab back to the message. The bar is portaled to the end of the body, so the
   * browser's own next stop would be BODY: the stop is chosen here, and the bar closes.
   */
  const leaveToolbar = (backward: boolean) => {
    const article = target?.article;
    reaction.close();
    if (backward) {
      if (article) reaction.restoreFocus(article);
      return;
    }
    const next = document.getElementById("composer-textarea") ?? nextFocusableAfter(document.getElementById("message-list"));
    if (next) reaction.restoreFocus(next);
  };

  const onBarKeyDown = (event: KeyboardEvent<HTMLDivElement>) => {
    if (event.key === "Tab") {
      event.preventDefault();
      leaveToolbar(event.shiftKey);
      return;
    }
    if (event.key === "Escape" && mode === "quick") {
      event.preventDefault();
      close(true);
      return;
    }
    if (mode !== "quick" || !["ArrowLeft", "ArrowRight", "Home", "End"].includes(event.key)) return;
    const buttons = Array.from(barRef.current?.querySelectorAll<HTMLElement>("button[data-reaction-quick], button[data-reaction-more]") ?? []);
    const index = buttons.indexOf(document.activeElement as HTMLElement);
    if (index < 0) return;
    event.preventDefault();
    const next = event.key === "Home" ? 0 : event.key === "End" ? buttons.length - 1 : index + (event.key === "ArrowRight" ? 1 : -1);
    const clamped = Math.max(0, Math.min(next, buttons.length - 1));
    setRovingIndex(clamped);
    buttons[clamped]?.focus();
  };

  if (hidden) return null;
  const Icon = notice ? NOTICE_ICON[notice] : undefined;
  const compact = quickCount < QUICK_REACTIONS.length;
  const busy = inFlight.has(message.id);
  const moreIndex = quick.length;

  return createPortal(<div id="message-reaction-layer" data-reaction-layer style={{ left: placement.left, top: placement.top }}>
    <div
      ref={barRef}
      id="message-reaction-bar"
      data-reaction-bar={message.id}
      data-reaction-mode={notice ? "notice" : mode}
      data-reaction-route={route}
      data-reaction-placement={placement.placement}
      data-compact={compact || undefined}
      data-reaction-busy={busy || undefined}
      role={notice ? undefined : "toolbar"}
      aria-label={notice ? undefined : toolbarLabel(route, message, target.article, nameOf)}
      onKeyDown={onBarKeyDown}
      onPointerEnter={reaction.keepOpen}
      onPointerLeave={() => { if (mode === "quick") reaction.scheduleClose(); }}
      onBlur={(event) => {
        const next = event.relatedTarget;
        if (next instanceof Node && !barRef.current?.contains(next)) reaction.close();
      }}
    >
      {notice && Icon && <span data-reaction-bar-notice={notice}><Icon size={12} aria-hidden />{NOTICE_COPY[notice]}</span>}
      {!notice && mode === "capture" && <ReactionCapture
        route={route}
        writesPaused={writesPaused}
        platform={detectPlatform()}
        coarse={coarse}
        openPicker={openPicker}
        onCommit={(emoji, multiple) => choose(emoji, true, multiple)}
        onBack={() => { focusMoreAfterBack.current = true; setMode("quick"); }}
        announce={announce}
      />}
      {!notice && mode === "quick" && <>
        {route === "text" && <span data-reaction-route-label="text"><MessageSquare size={12} aria-hidden />Text</span>}
        {quick.map(({ emoji, name, hook }, index) => <button
          key={emoji}
          type="button"
          data-reaction-quick={hook}
          aria-label={name}
          aria-pressed={route === "native" ? emoji === selected : undefined}
          aria-disabled={writesPaused || undefined}
          aria-describedby={writesPaused ? "update-paused-description" : undefined}
          title={writesPaused ? UPDATE_PAUSED_DESCRIPTION : undefined}
          tabIndex={index === tabStop ? 0 : -1}
          onFocus={() => setRovingIndex(index)}
          onClick={() => choose(emoji, false)}
        >{emoji}</button>)}
        <button
          type="button"
          data-reaction-more
          aria-label="More emoji"
          aria-disabled={writesPaused || undefined}
          aria-describedby={writesPaused ? "update-paused-description" : undefined}
          title={writesPaused ? UPDATE_PAUSED_DESCRIPTION : undefined}
          tabIndex={moreIndex === tabStop ? 0 : -1}
          onFocus={() => setRovingIndex(moreIndex)}
          onClick={() => { if (!writesPaused) setMode("capture"); }}
        ><SmilePlus size={18} aria-hidden /></button>
      </>}
    </div>
  </div>, document.body);
}
