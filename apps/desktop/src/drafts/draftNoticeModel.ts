export type DraftNoticeKind =
  | "save-failed"
  | "stale-send"
  | "send-failed"
  | "moved"
  | "remote-update"
  | "replaced"
  | "saved"
  | "saving"
  | "loading";

export type DraftNoticeTone = "neutral" | "warning" | "error";
export type DraftNoticeAction = "retry" | "undo" | "copy";

export type DraftNoticeView = {
  kind: DraftNoticeKind;
  message: string;
  tone: DraftNoticeTone;
  role: "alert" | "status";
  action?: DraftNoticeAction;
  durationMs?: number;
};

// Recovery flags are read only for recovery notices and stale sends.
// Undo wins when both are set; Copy text is offered only for retained text
// that cannot be undone, such as a structural catch-up that changed recipients.
type DraftNoticeBase = {
  sequence: number;
  detail?: string;
  undoAvailable?: boolean;
  retainedTextAvailable?: boolean;
};

export type DraftNoticeCandidate = DraftNoticeBase & (
  | { kind: "send-failed"; detail: string }
  | { kind: "moved"; detail: string }
  | { kind: Exclude<DraftNoticeKind, "send-failed" | "moved"> }
);

const SAVED_DURATION_MS = 2000;
const REMOTE_UPDATE_DURATION_MS = 4000;
const MOVED_DURATION_MS = 5000;
const REPLACED_DURATION_MS = 6000;

// Lower rank wins. Equal ranks fall back to the newest sequence.
const PRIORITY_RANK: Record<DraftNoticeKind, number> = {
  "save-failed": 0,
  "stale-send": 1,
  "send-failed": 2,
  moved: 3,
  "remote-update": 4,
  replaced: 4,
  saved: 5,
  saving: 6,
  loading: 7,
};

export function selectDraftNotice(candidates: readonly DraftNoticeCandidate[]): DraftNoticeView | undefined {
  let winner: DraftNoticeCandidate | undefined;
  for (const candidate of candidates) {
    if (!winner || outranks(candidate, winner)) winner = candidate;
  }
  return winner && draftNoticeView(winner);
}

export function draftNoticeView(candidate: DraftNoticeCandidate): DraftNoticeView {
  switch (candidate.kind) {
    case "save-failed":
      return {
        kind: candidate.kind,
        message: candidate.detail ?? "Draft not saved. Your text is kept. Sending and closing wait until it saves.",
        tone: "error",
        role: "alert",
        action: "retry",
      };
    case "stale-send":
      return {
        kind: candidate.kind,
        message: "Not sent. This draft changed in another window. Review it, then send again.",
        tone: "warning",
        role: "alert",
        ...recoveryAction(candidate),
      };
    case "send-failed":
      return { kind: candidate.kind, message: `Not sent: ${candidate.detail}`, tone: "error", role: "alert" };
    case "moved":
      return {
        kind: candidate.kind,
        message: `Draft added to your existing conversation with ${candidate.detail}.`,
        tone: "neutral",
        role: "status",
        durationMs: MOVED_DURATION_MS,
        ...recoveryAction(candidate),
      };
    case "remote-update":
      return {
        kind: candidate.kind,
        message: "Updated from another window.",
        tone: "neutral",
        role: "status",
        durationMs: REMOTE_UPDATE_DURATION_MS,
        ...recoveryAction(candidate),
      };
    case "replaced":
      return {
        kind: candidate.kind,
        message: "Replaced text from another window.",
        tone: "neutral",
        role: "status",
        durationMs: REPLACED_DURATION_MS,
        ...recoveryAction(candidate),
      };
    case "saved":
      return { kind: candidate.kind, message: "Draft saved.", tone: "neutral", role: "status", durationMs: SAVED_DURATION_MS };
    case "saving":
      return { kind: candidate.kind, message: "Saving…", tone: "neutral", role: "status" };
    case "loading":
      return { kind: candidate.kind, message: "Loading draft…", tone: "neutral", role: "status" };
  }
}

function recoveryAction(candidate: DraftNoticeCandidate): { action?: DraftNoticeAction } {
  if (candidate.kind !== "stale-send" && candidate.undoAvailable) return { action: "undo" };
  if (candidate.retainedTextAvailable) return { action: "copy" };
  return {};
}

function outranks(candidate: DraftNoticeCandidate, current: DraftNoticeCandidate): boolean {
  const rankDifference = PRIORITY_RANK[candidate.kind] - PRIORITY_RANK[current.kind];
  return rankDifference < 0 || (rankDifference === 0 && candidate.sequence > current.sequence);
}
