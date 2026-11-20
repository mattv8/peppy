import { useEffect, useLayoutEffect, useRef, useState, type ReactNode } from "react";
import { BellOff, Clock, UPDATE_PAUSED_DESCRIPTION, formatDateTime, formatElapsedShort, isoDateTime, X } from "@peppy/desktop-ui";
import type { AppFilter, MirroredNotification, NotificationTarget } from "../bridge/bridge";
import type { OneTimeCodeTarget } from "@peppy/desktop-ui/bridge-contracts";
import { OneTimeCodeButton } from "../one-time-code/OneTimeCodeButton";
import { notificationMuteKey, notificationTuple } from "./notificationIdentity";
import { highlightSegments, notificationSearchTerms } from "./notificationSearch";

export type NotificationLabels = {
  appName(notification: MirroredNotification): string;
  sourceName(notification: MirroredNotification): string;
  title(notification: MirroredNotification): string;
};

export type NotificationRowProps = {
  writesPaused?: boolean;
  notification: MirroredNotification;
  labels: NotificationLabels;
  filter: AppFilter;
  unread: boolean;
  showPhone: boolean;
  query: string;
  onDismiss(target: NotificationTarget): void;
  onMute(filter: AppFilter): void;
  onCopy(target: OneTimeCodeTarget, code: string): Promise<void>;
  onAction(keys: string[]): void;
  hideAppName?: boolean;
};

export function NotificationAppIcon({ appName, base64 }: { appName: string; base64?: string }) {
  const [failed, setFailed] = useState(false);
  useEffect(() => setFailed(false), [base64]);
  if (!base64 || failed) {
    return <span className="notif-app-icon-fallback" data-notification-app-icon="fallback" aria-hidden>
      {Array.from(appName)[0]?.toUpperCase() ?? "A"}
    </span>;
  }
  return <img className="notif-app-icon" data-notification-app-icon="image" src={`data:image/png;base64,${base64}`} alt="" onError={() => setFailed(true)} />;
}

function highlightedText(text: string, query: string): ReactNode[] {
  return highlightSegments(text, notificationSearchTerms(query)).map((segment, index) =>
    segment.match ? <mark key={index}>{segment.text}</mark> : segment.text,
  );
}

function NotificationRowText({ id, text, query }: { id: string; text: string; query: string }) {
  const [expanded, setExpanded] = useState(false);
  const [overflowing, setOverflowing] = useState(false);
  const textRef = useRef<HTMLSpanElement>(null);
  const clamped = !expanded && !query.trim();
  useLayoutEffect(() => {
    const el = textRef.current;
    if (!el || !clamped) return;
    const measure = () => setOverflowing(el.scrollHeight > el.clientHeight + 1);
    measure();
    if (typeof ResizeObserver === "undefined") return;
    const observer = new ResizeObserver(measure);
    observer.observe(el);
    return () => observer.disconnect();
  }, [clamped, text]);
  return <>
    <span id={id} ref={textRef} className="notif-row-text" data-clamped={clamped || undefined}>{highlightedText(text, query)}</span>
    {!query.trim() && (overflowing || expanded) && <button type="button" className="notif-row-more" aria-expanded={expanded} aria-controls={id} onClick={() => setExpanded(!expanded)}>{expanded ? "Show less" : "Show more"}</button>}
  </>;
}

export function NotificationRow({
  notification, labels, filter, unread, showPhone, query, onDismiss, onMute, onCopy, onAction, hideAppName, writesPaused = false,
}: NotificationRowProps) {
  const pending = notification.dismissalPending;
  const key = notificationTuple(notification.target.sourceDeviceId, notification.target.notificationKey, notification.target.lifetime);
  const appName = labels.appName(notification);
  const phoneName = labels.sourceName(notification);
  const title = labels.title(notification);
  const muteLabel = `Mute ${appName}${showPhone ? ` on ${phoneName}` : ""}`;
  const dismissLabel = `Dismiss ${title || appName || "notification"} from ${appName} on ${phoneName}`;
  const titleId = `notif-title-${key}`;
  const metaId = `notif-meta-${key}`;
  const dismissRequested = useRef(false);
  const dismissButton = useRef<HTMLButtonElement>(null);
  useEffect(() => {
    const active = document.activeElement;
    if (!pending || !dismissRequested.current || (active !== document.body && active !== dismissButton.current)) return;
    document.getElementById(`notif-${key}`)?.focus({ preventScroll: true });
    dismissRequested.current = false;
  }, [key, pending]);

  return <article
    id={`notif-${key}`}
    tabIndex={-1}
    data-device-id={notification.target.sourceDeviceId}
    data-notification-key={notification.target.notificationKey}
    data-dismiss-state={pending ? "pending" : "idle"}
    data-read-state={unread ? "unread" : "read"}
    data-notification-target={JSON.stringify(notification.target)}
    className="notif-row"
    aria-labelledby={titleId}
    aria-describedby={metaId}
  >
    <NotificationAppIcon appName={appName} base64={notification.appIconPngBase64} />
    <div className="notif-row-body" id={metaId}>
      {unread && <span className="visually-hidden">Unread, </span>}
      <span className="notif-row-app-source" data-stack-app-name-hidden={hideAppName || undefined}>
        {!hideAppName && <b>{appName}</b>}
        {showPhone && <span className="notif-phone-badge">{phoneName}</span>}
      </span>
      <b id={titleId} className="notif-row-title">{highlightedText(title, query)}</b>
      <NotificationRowText id={`notif-text-${key}`} text={notification.text} query={query} />
      {notification.oneTimeCode && <OneTimeCodeButton
        key={`${key}:${notification.oneTimeCode}`}
        variant="notification"
        hookId={key}
        target={{ kind: "notification", target: notification.target }}
        code={notification.oneTimeCode}
        contextName={appName}
        onCopy={onCopy}
      />}
      {pending && <span className="notif-pending" title="Dismissal pending — will apply on your phone's next sync."><Clock size={14} aria-hidden />Waiting for phone</span>}
    </div>
    <div className="notif-row-side">
      <time className="notif-row-time" dateTime={isoDateTime(notification.postedAt)} title={formatDateTime(notification.postedAt) ?? "Unknown time"}>
        {formatElapsedShort(notification.postedAt) ?? "Unknown time"}
      </time>
      <div className="notif-row-actions">
        <button className="notif-inline-action" title={writesPaused ? UPDATE_PAUSED_DESCRIPTION : muteLabel} aria-label={muteLabel} disabled={writesPaused} aria-describedby={writesPaused ? "update-paused-description" : undefined} onClick={() => { onAction([key]); onMute(filter); }}>
          <BellOff size={14} aria-hidden />
        </button>
        <button ref={dismissButton} className="notif-dismiss-btn" disabled={pending || !notification.dismissible} title={pending ? "Dismissal pending — will apply on your phone's next sync." : "Dismiss"} aria-label={dismissLabel} onClick={() => { dismissRequested.current = true; onAction([key]); onDismiss(notification.target); }}>
          <X size={14} aria-hidden />
        </button>
      </div>
    </div>
  </article>;
}
