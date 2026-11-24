import type { NotificationDismissalBlockReason } from "@peppy/desktop-ui/bridge-contracts";

const MINUTE_MS = 60_000;
const HOUR_MS = 60 * MINUTE_MS;
const DAY_MS = 24 * HOUR_MS;

/** Short age of a phone's report: "just now", then "Nm", "Nh" or "Nd". Future timestamps count as now. */
export function formatReportAge(observedAtMs: number, nowMs: number): string {
  const elapsed = Math.max(0, nowMs - observedAtMs);
  if (elapsed < MINUTE_MS) return "just now";
  if (elapsed < HOUR_MS) return `${Math.floor(elapsed / MINUTE_MS)}m`;
  if (elapsed < DAY_MS) return `${Math.floor(elapsed / HOUR_MS)}h`;
  return `${Math.floor(elapsed / DAY_MS)}d`;
}

export function reportedText(observedAtMs: number, nowMs: number): string {
  const age = formatReportAge(observedAtMs, nowMs);
  return age === "just now" ? "Reported just now." : `Reported ${age} ago.`;
}

export const WAITING_PANEL_TITLE = "Waiting for your phone";

export function waitingCountLabel(count: number): string {
  return `${count} waiting for phone`;
}

export function waitingHelpParagraphs(count: number): string[] {
  return [
    "You dismissed these here, so Peppy hides them. Your phone clears each one the next time it connects. Peppy can't say when that will be.",
    "Some can take longer. If an app keeps updating a notification quietly, your phone may only clear it when the app sends its next alert.",
    "A phone running an older version of Peppy can't tell this computer when something is wrong. Update Peppy on the phone if these stay for a long time.",
    `Choose \u201C${waitingCountLabel(count)}\u201D to see them. You can't undo a dismissal. If an app puts one back, it returns to your list with the reason.`,
  ];
}

export const WAITING_HELP_LABEL = "About notifications waiting for phone";
export const WAITING_GROUP_LABEL = "Notifications waiting for phone";

export const WAITING_SHOWN_ANNOUNCEMENT = (count: number) =>
  `${count} waiting notifications shown at the top of the list. They can't be undone.`;
export const WAITING_HIDDEN_ANNOUNCEMENT = "Waiting notifications hidden.";
export const NO_WAITING_ANNOUNCEMENT = "No notifications are waiting for phone.";

export const waitingRowStatus = (phoneName: string) => `Dismissed, waiting for ${phoneName}`;
export const waitingGroupHeader = (phoneName: string) => `Dismissed, waiting for ${phoneName}`;
export const WAITING_GROUP_NOTE = (phoneName: string) => `You can't undo or cancel these. ${phoneName} hasn't cleared them yet.`;
export const BLOCKER_GROUP_NOTE = (phoneName: string) => `You can't undo or cancel these. ${phoneName} will clear them once it can.`;

export const refusedRowStatus = (appName: string) => `Can't be dismissed. ${appName} now keeps it pinned on your phone.`;

export const UPDATE_IN_PROGRESS_REASON = "Peppy is updating your messages.";
export const UPDATE_BUSY_REASON = "Peppy is about to restart for an update. Dismiss again after it reopens.";
export const LOCKED_REASON = "Peppy is locked. Unlock it, then try again.";
export const GENERIC_FAILURE_REASON = "Peppy couldn't save this.";

export const localFailureText = (reason: string) => `Couldn't dismiss. ${reason}`;
export const bulkFailureText = (count: number, reason: string) => `Couldn't dismiss ${count} notifications. ${reason}`;
export const TRY_AGAIN_LABEL = "Try again";
export const tryAgainLabel = (title: string, appName: string) => `Try again to dismiss ${title} from ${appName}`;

export const dismissedAnnouncement = (title: string, appName: string) => `Dismissed ${title} from ${appName}.`;
export const dismissedManyAnnouncement = (count: number) => `Dismissed ${count} notifications.`;
/** Contract 7 wording, shared by the hook and the view. */
export const notDismissibleAnnouncement = (title: string) => `${title} can't be dismissed. It's back in the list.`;
export const notDismissibleManyAnnouncement = (count: number) => `${count} notifications can't be dismissed. They're back in the list.`;
export const blockedDismissAnnouncement = (title: string, appName: string, phoneName: string) =>
  `Dismissed ${title} from ${appName}. ${phoneName} can't clear it until the notice above is resolved.`;
export const BLOCKED_SHOWN_ANNOUNCEMENT = (count: number) => `${count} dismissed notifications shown. They can't be undone.`;
export const BLOCKED_HIDDEN_ANNOUNCEMENT = "Dismissed notifications hidden again.";

const waitingSentence = (count: number) =>
  count === 1 ? "1 dismissed notification is waiting." : `${count} dismissed notifications are waiting.`;

export const blockerRevealLabel = (count: number, expanded: boolean) =>
  `${expanded ? "Hide" : "Show"} ${count} dismissed`;

export const BLOCKER_HELP_CLOSING = (phoneName: string) =>
  `This notice goes away after ${phoneName} reports back. A phone that is locked or offline can't report, so it can take a while.`;

export type BlockerCopy = {
  title: string;
  body: string;
  /** Short fix sentence, used in the grouped list. */
  fix: string;
  help: { title: string; steps: string[]; paragraphs: string[] };
};

/** Copy for one blocked phone. The `reason` drives the wording; `other` covers unknown reasons. */
export function blockerCopy(reason: NotificationDismissalBlockReason, phoneName: string, waitingCount: number): BlockerCopy {
  const waiting = waitingSentence(waitingCount);
  const closing = BLOCKER_HELP_CLOSING(phoneName);
  const closingLine = `${phoneName} will then try to clear the dismissed notifications.`;
  switch (reason) {
    case "notificationAccessOff":
      return {
        title: `Notification access is off on ${phoneName}`,
        body: `${waiting} Turn notification access back on for Peppy in the phone's settings, and ${phoneName} will try to clear them.`,
        fix: "Notification access is off. Turn it on for Peppy in the phone's settings.",
        help: {
          title: "Turn on notification access",
          steps: [`On ${phoneName}, open Settings.`, "Search for \u201CNotification access\u201D.", "Open it and turn Peppy on."],
          paragraphs: [closingLine, closing],
        },
      };
    case "mirroringOff":
      return {
        title: `Notification mirroring is off on ${phoneName}`,
        body: `${waiting} Turn on Notification mirroring in Peppy on the phone, and ${phoneName} will try to clear them.`,
        fix: "Notification mirroring is off. Turn it on in Peppy on the phone.",
        help: {
          title: "Turn notification mirroring back on",
          steps: [`On ${phoneName}, open Peppy.`, "Open Settings.", "Turn on Notification mirroring."],
          paragraphs: [closingLine, closing],
        },
      };
    case "waitingForWifi":
      return {
        title: `${phoneName} is waiting for Wi-Fi`,
        body: `${waiting} Notification mirroring is set to Wi-Fi only and the phone is using mobile data. Nothing to do: ${phoneName} will try again on Wi-Fi.`,
        fix: "Waiting for Wi-Fi. Nothing to do; it will try again on Wi-Fi.",
        help: {
          title: "Wi-Fi only",
          steps: [],
          paragraphs: [
            `Peppy on ${phoneName} only mirrors notifications over Wi-Fi, and the phone is using mobile data right now. It will try again when it is back on Wi-Fi. You don't need to do anything.`,
            "To change this, open Peppy on the phone, open Settings, and turn off Wi-Fi only under Notification mirroring.",
            closing,
          ],
        },
      };
    case "other":
    default:
      return {
        title: `${phoneName} can't clear notifications right now`,
        body: `${waiting} Open Peppy on the phone to check its notification settings.`,
        fix: "Something on the phone is stopping it. Open Peppy on the phone to check.",
        help: {
          title: "Check the phone",
          steps: [],
          paragraphs: [`Open Peppy on ${phoneName} and check Notification mirroring.`, closing],
        },
      };
  }
}

export const blockerReasonSeverity = (reason: NotificationDismissalBlockReason): "warning" | "info" =>
  reason === "waitingForWifi" ? "info" : "warning";

/** Grouped banner title for three or more blocked phones. */
export function groupedBlockersTitle(count: number, allWifi: boolean): string {
  if (allWifi) return `${count} phones are waiting for Wi-Fi`;
  return `${count} phones can't clear notifications`;
}

export function joinNames(names: string[]): string {
  if (names.length <= 1) return names[0] ?? "";
  return `${names.slice(0, -1).join(", ")} and ${names[names.length - 1]}`;
}

export function groupedBlockersBody(names: string[], totalWaiting: number): string {
  const verb = totalWaiting === 1 ? "has 1 dismissed notification" : `have ${totalWaiting} dismissed notifications`;
  return `${joinNames(names)} ${verb} waiting. Show phones to see what each one needs.`;
}

export const groupedBlockersToggleLabel = (expanded: boolean) => (expanded ? "Hide phones" : "Show phones");
