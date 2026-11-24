import { useEffect, useState, type ReactElement } from "react";
import { ChevronDown, Info, TriangleAlert, isoDateTime } from "@peppy/desktop-ui";
import type { NotificationDismissalBlockReason } from "@peppy/desktop-ui/bridge-contracts";
import { NotificationDismissHelp } from "./NotificationDismissHelp";
import {
  blockerCopy, blockerReasonSeverity, blockerRevealLabel, groupedBlockersBody, groupedBlockersTitle, groupedBlockersToggleLabel, reportedText,
} from "./notificationDismissCopy";

export type PhoneBlock = {
  phoneKey: string;
  phoneName: string;
  reason: NotificationDismissalBlockReason;
  observedAtMs: number;
  waitingCount: number;
};

export type NotificationPhoneBlockersProps = {
  blocks: PhoneBlock[];
  expandedPhones: ReadonlySet<string>;
  onTogglePhone(phoneKey: string): void;
  /** Fixed clock for the report ages, for tests. Without it, ages use the current time and refresh every minute. */
  nowMs?: number;
};

const GROUP_THRESHOLD = 3;
const AGE_REFRESH_MS = 60_000;

function ReasonIcon({ reason, size = 16 }: { reason: NotificationDismissalBlockReason; size?: number }) {
  return reason === "waitingForWifi" ? <Info size={size} aria-hidden /> : <TriangleAlert size={size} aria-hidden />;
}

function RevealButton({ block, expanded, onTogglePhone }: Pick<PhoneBlockerProps, "block" | "expanded" | "onTogglePhone">) {
  if (block.waitingCount <= 0) return null;
  return <button id={`phone-blocker-${block.phoneKey}-toggle`} type="button" className="secondary-button notification-phone-blocker-toggle" aria-expanded={expanded} aria-controls={`notifications-hidden-${block.phoneKey}`} onClick={() => onTogglePhone(block.phoneKey)}>
    <ChevronDown size={14} aria-hidden className="notification-phone-blocker-chevron" data-expanded={expanded || undefined} />
    {blockerRevealLabel(block.waitingCount, expanded)}
  </button>;
}

type PhoneBlockerProps = {
  block: PhoneBlock;
  expanded: boolean;
  onTogglePhone(phoneKey: string): void;
  nowMs: number;
};

function BlockerHelp({ block }: { block: PhoneBlock }) {
  const copy = blockerCopy(block.reason, block.phoneName, block.waitingCount);
  return <NotificationDismissHelp triggerId={`phone-blocker-${block.phoneKey}-help`} triggerLabel={`About ${copy.title}`} help={{ kind: "blocker", reason: block.reason, phoneName: block.phoneName }} />;
}

/** One banner for a blocked phone, in the system-ribbon look. */
function PhoneBlockerBanner({ block, expanded, onTogglePhone, nowMs }: PhoneBlockerProps): ReactElement {
  const copy = blockerCopy(block.reason, block.phoneName, block.waitingCount);
  return <section id={`phone-blocker-${block.phoneKey}`} className="system-ribbon notification-phone-blocker" data-phone-blocker data-blocker-reason={block.reason} data-system-ribbon-severity={blockerReasonSeverity(block.reason)}>
    <ReasonIcon reason={block.reason} />
    <div className="system-ribbon-text notification-phone-blocker-text">
      <div className="notification-phone-blocker-title"><strong>{copy.title}</strong><BlockerHelp block={block} /></div>
      <span>{copy.body}</span>
      <time dateTime={isoDateTime(block.observedAtMs)}>{reportedText(block.observedAtMs, nowMs)}</time>
    </div>
    <div className="app-notice-actions">
      <RevealButton block={block} expanded={expanded} onTogglePhone={onTogglePhone} />
    </div>
  </section>;
}

/** One row of the grouped list: the fix sentence and meta line, with its own help and reveal toggle. */
function PhoneBlockerListItem({ block, expanded, onTogglePhone, nowMs }: PhoneBlockerProps): ReactElement {
  const copy = blockerCopy(block.reason, block.phoneName, block.waitingCount);
  return <li id={`phone-blocker-${block.phoneKey}`} data-phone-blocker data-blocker-reason={block.reason} className="notification-phone-blocker-item">
    <ReasonIcon reason={block.reason} size={14} />
    <div className="notification-phone-blocker-item-text">
      <div className="notification-phone-blocker-title"><strong>{block.phoneName}</strong><BlockerHelp block={block} /></div>
      <span>{copy.fix}</span>
      <span className="notification-phone-blocker-meta">
        {block.waitingCount > 0 && <span>{block.waitingCount} waiting</span>}
        <time dateTime={isoDateTime(block.observedAtMs)}>{reportedText(block.observedAtMs, nowMs)}</time>
      </span>
    </div>
    <RevealButton block={block} expanded={expanded} onTogglePhone={onTogglePhone} />
  </li>;
}

type GroupedPhoneBlockersProps = Omit<NotificationPhoneBlockersProps, "nowMs"> & { nowMs: number };

function GroupedPhoneBlockers({ blocks, expandedPhones, onTogglePhone, nowMs }: GroupedPhoneBlockersProps): ReactElement {
  const [listOpen, setListOpen] = useState(false);
  const allWifi = blocks.every(block => block.reason === "waitingForWifi");
  const total = blocks.reduce((sum, block) => sum + block.waitingCount, 0);
  return <section id="phone-blockers-group" className="system-ribbon notification-phone-blocker-group" data-phone-blocker-group={blocks.length} data-system-ribbon-severity={allWifi ? "info" : "warning"}>
    {allWifi ? <Info size={16} aria-hidden /> : <TriangleAlert size={16} aria-hidden />}
    <div className="system-ribbon-text notification-phone-blocker-text">
      <strong>{groupedBlockersTitle(blocks.length, allWifi)}</strong>
      <span>{groupedBlockersBody(blocks.map(block => block.phoneName), total)}</span>
    </div>
    <div className="app-notice-actions">
      <button id="phone-blockers-group-toggle" type="button" className="secondary-button notification-phone-blocker-toggle" aria-expanded={listOpen} aria-controls="phone-blockers-list" onClick={() => setListOpen(!listOpen)}>
        <ChevronDown size={14} aria-hidden className="notification-phone-blocker-chevron" data-expanded={listOpen || undefined} />
        {groupedBlockersToggleLabel(listOpen)}
      </button>
    </div>
    <ul id="phone-blockers-list" className="notification-phone-blocker-list" aria-label="Blocked phones" hidden={!listOpen}>
      {listOpen && blocks.map(block => <PhoneBlockerListItem key={block.phoneKey} block={block} expanded={expandedPhones.has(block.phoneKey)} onTogglePhone={onTogglePhone} nowMs={nowMs} />)}
    </ul>
  </section>;
}

/** Banners for phones that report a reason they cannot clear dismissed notifications. Nothing when no phone is blocked. */
export function NotificationPhoneBlockers({ blocks, expandedPhones, onTogglePhone, nowMs: fixedNowMs }: NotificationPhoneBlockersProps): ReactElement | null {
  const [, setAgeTick] = useState(0);
  const showing = blocks.length > 0;
  useEffect(() => {
    if (!showing) return;
    const timer = window.setInterval(() => setAgeTick(tick => tick + 1), AGE_REFRESH_MS);
    return () => window.clearInterval(timer);
  }, [showing]);
  if (!showing) return null;
  const nowMs = fixedNowMs ?? Date.now();
  return <div id="notifications-phone-blockers" className="notification-phone-blockers">
    {blocks.length < GROUP_THRESHOLD
      ? blocks.map(block => <PhoneBlockerBanner key={block.phoneKey} block={block} expanded={expandedPhones.has(block.phoneKey)} onTogglePhone={onTogglePhone} nowMs={nowMs} />)
      : <GroupedPhoneBlockers blocks={blocks} expandedPhones={expandedPhones} onTogglePhone={onTogglePhone} nowMs={nowMs} />}
  </div>;
}
