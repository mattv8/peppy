import "@testing-library/jest-dom/vitest";
import { act, cleanup, fireEvent, render, screen } from "@testing-library/react";
import { afterEach, describe, expect, it, vi } from "vitest";
import { DraftNotice } from "./DraftNotice";
import { draftNoticeView, selectDraftNotice, type DraftNoticeCandidate, type DraftNoticeKind } from "./draftNoticeModel";

const PRIORITY_ORDER: readonly DraftNoticeKind[] = [
  "save-failed",
  "stale-send",
  "send-failed",
  "moved",
  "remote-update",
  "saved",
  "saving",
  "loading",
];
const NON_RETRY_KINDS = PRIORITY_ORDER.filter(kind => kind !== "save-failed");
const RECOVERY_KINDS = ["remote-update", "replaced", "moved"] as const;

type RecoveryOptions = { undoAvailable?: boolean; retainedTextAvailable?: boolean };

function candidateOf(kind: DraftNoticeKind, sequence: number, options: RecoveryOptions = {}): DraftNoticeCandidate {
  switch (kind) {
    case "send-failed":
    case "moved":
      return { kind, sequence, detail: "Recipient", ...options };
    default:
      return { kind, sequence, ...options };
  }
}

describe("selectDraftNotice", () => {
  afterEach(cleanup);

  it("returns nothing for a healthy draft", () => {
    expect(selectDraftNotice([])).toBeUndefined();
  });

  it("lets each kind win over every lower-priority kind regardless of recency", () => {
    PRIORITY_ORDER.forEach((higher, index) => {
      const lower = PRIORITY_ORDER[index + 1];
      if (!lower) return;
      expect(selectDraftNotice([candidateOf(lower, 2), candidateOf(higher, 1)])?.kind).toBe(higher);
    });
  });

  it("ranks remote updates and replacements equally, so the newest wins", () => {
    expect(selectDraftNotice([candidateOf("remote-update", 1), candidateOf("replaced", 2)])?.kind).toBe("replaced");
    expect(selectDraftNotice([candidateOf("replaced", 1), candidateOf("remote-update", 2)])?.kind).toBe("remote-update");
  });

  it("keeps the newest of equal-priority candidates", () => {
    expect(selectDraftNotice([candidateOf("saved", 3), candidateOf("saved", 7)])?.kind).toBe("saved");
    expect(selectDraftNotice([candidateOf("saving", 9), candidateOf("saving", 4)])).toEqual(draftNoticeView(candidateOf("saving", 9)));
  });

  it("uses failure tones and alert roles for failures and stale sends", () => {
    expect(draftNoticeView(candidateOf("save-failed", 1))).toMatchObject({ tone: "error", role: "alert" });
    expect(draftNoticeView(candidateOf("send-failed", 1))).toMatchObject({ tone: "error", role: "alert" });
    expect(draftNoticeView(candidateOf("stale-send", 1))).toMatchObject({ tone: "warning", role: "alert" });
  });

  it("uses status roles for quiet notices", () => {
    for (const kind of ["moved", "remote-update", "replaced", "saved", "saving", "loading"] as const) {
      expect(draftNoticeView(candidateOf(kind, 1)).role).toBe("status");
    }
  });

  it("offers Retry only for save failures", () => {
    expect(draftNoticeView(candidateOf("save-failed", 1)).action).toBe("retry");
    for (const kind of NON_RETRY_KINDS) {
      expect(draftNoticeView(candidateOf(kind, 1)).action).not.toBe("retry");
    }
  });

  it("keeps setup failures actionable without calling them unsaved drafts", () => {
    const notice = draftNoticeView({ kind: "save-failed", detail: "Draft updates unavailable: listener failed", sequence: 1 });
    expect(notice).toMatchObject({ action: "retry", message: "Draft updates unavailable: listener failed" });
  });

  it("offers explicit recovery for a deleted draft", () => {
    const notice = draftNoticeView({ kind: "save-failed", errorCode: "not-found", sequence: 1 });
    expect(notice).toMatchObject({ action: "save-new", message: expect.stringContaining("no longer exists") });
  });

  it("offers Undo for recovery notices only when undo is available", () => {
    for (const kind of RECOVERY_KINDS) {
      expect(draftNoticeView(candidateOf(kind, 1, { undoAvailable: true })).action).toBe("undo");
      expect(draftNoticeView(candidateOf(kind, 1)).action).toBeUndefined();
    }
  });

  it("offers Copy for recovery notices when only retained text is available", () => {
    for (const kind of RECOVERY_KINDS) {
      expect(draftNoticeView(candidateOf(kind, 1, { retainedTextAvailable: true })).action).toBe("copy");
    }
  });

  it("prefers Undo over Copy when both are available", () => {
    for (const kind of RECOVERY_KINDS) {
      expect(draftNoticeView(candidateOf(kind, 1, { undoAvailable: true, retainedTextAvailable: true })).action).toBe("undo");
    }
  });

  it("offers Copy for stale sends and recovery notices only when retained text is available", () => {
    expect(draftNoticeView(candidateOf("stale-send", 1, { retainedTextAvailable: true })).action).toBe("copy");
    for (const kind of PRIORITY_ORDER.filter(kind => kind !== "stale-send" && !(RECOVERY_KINDS as readonly DraftNoticeKind[]).includes(kind))) {
      expect(draftNoticeView(candidateOf(kind, 1, { retainedTextAvailable: true })).action).not.toBe("copy");
    }
  });

  it("gives transient notices their lifetimes and keeps failures and pending states open", () => {
    expect(draftNoticeView(candidateOf("saved", 1)).durationMs).toBe(2000);
    expect(draftNoticeView(candidateOf("remote-update", 1)).durationMs).toBe(4000);
    expect(draftNoticeView(candidateOf("replaced", 1)).durationMs).toBe(6000);
    expect(draftNoticeView(candidateOf("moved", 1)).durationMs).toBe(5000);
    for (const kind of ["save-failed", "stale-send", "send-failed", "saving", "loading"] as const) {
      expect(draftNoticeView(candidateOf(kind, 1)).durationMs).toBeUndefined();
    }
  });

  it("names the conversation for moved drafts and the reason for failed sends", () => {
    expect(draftNoticeView({ kind: "moved", sequence: 1, detail: "Ada" }).message).toContain("Ada");
    expect(draftNoticeView({ kind: "send-failed", sequence: 1, detail: "No route" }).message).toContain("No route");
  });
});

describe("DraftNotice", () => {
  afterEach(() => {
    cleanup();
    vi.useRealTimers();
  });

  it("renders nothing for a healthy draft", () => {
    const { container } = render(<DraftNotice notice={undefined} onAction={vi.fn()} />);
    expect(container).toBeEmptyDOMElement();
  });

  it("renders the stable hook with the notice role and tone", () => {
    render(<DraftNotice notice={draftNoticeView(candidateOf("saved", 1))} onAction={vi.fn()} />);
    const notice = document.querySelector("#draft-notice");
    expect(notice).toHaveAttribute("data-draft-notice", "saved");
    expect(notice).toHaveAttribute("data-kind", "saved");
    expect(notice).toHaveAttribute("role", "status");
    expect(screen.queryByRole("button")).toBeNull();
  });

  it("announces failures as alerts", () => {
    render(<DraftNotice notice={draftNoticeView(candidateOf("save-failed", 1))} onAction={vi.fn()} />);
    expect(screen.getByRole("alert")).toHaveAttribute("data-kind", "save-failed");
  });

  it("replaces the live region when a status becomes an alert", () => {
    const { rerender } = render(<DraftNotice notice={draftNoticeView(candidateOf("saved", 1))} onAction={vi.fn()} />);
    const status = screen.getByRole("status");
    rerender(<DraftNotice notice={draftNoticeView(candidateOf("save-failed", 2))} onAction={vi.fn()} />);
    expect(status.isConnected).toBe(false);
    expect(screen.getByRole("alert")).toHaveAttribute("data-kind", "save-failed");
  });

  it("reports the requested action without moving focus", () => {
    const onAction = vi.fn();
    render(<DraftNotice notice={draftNoticeView(candidateOf("save-failed", 1))} onAction={onAction} />);
    expect(document.activeElement).toBe(document.body);
    fireEvent.click(screen.getByRole("button", { name: "Retry" }));
    expect(onAction).toHaveBeenCalledWith("retry");
  });

  it("renders the explicit deleted-draft recovery action with a stable hook", () => {
    const onAction = vi.fn();
    render(<DraftNotice notice={draftNoticeView({ kind: "save-failed", errorCode: "not-found", sequence: 1 })} onAction={onAction} />);
    const action = screen.getByRole("button", { name: "Save as new draft" });
    expect(action).toHaveAttribute("data-draft-action", "save-new");
    fireEvent.click(action);
    expect(onAction).toHaveBeenCalledWith("save-new");
  });

  it("names the retained-text action for copy and reports it", () => {
    const onAction = vi.fn();
    render(<DraftNotice notice={draftNoticeView(candidateOf("replaced", 1, { retainedTextAvailable: true }))} onAction={onAction} />);
    const copy = screen.getByRole("button", { name: "Copy text of retained draft" });
    expect(copy).toHaveTextContent("Copy text");
    fireEvent.click(copy);
    expect(onAction).toHaveBeenCalledWith("copy");
  });

  it("keeps a busy action focusable but inert", () => {
    const onAction = vi.fn();
    const notice = draftNoticeView(candidateOf("remote-update", 1, { undoAvailable: true }));
    render(<DraftNotice notice={notice} onAction={onAction} busy />);
    const action = screen.getByRole("button", { name: "Undo" });
    expect(action).toHaveAttribute("aria-disabled", "true");
    expect(action).not.toBeDisabled();
    action.focus();
    expect(action).toHaveFocus();
    fireEvent.click(action);
    expect(onAction).not.toHaveBeenCalled();
  });

  it("keeps a busy copy action inert", () => {
    const onAction = vi.fn();
    render(<DraftNotice notice={draftNoticeView(candidateOf("moved", 1, { retainedTextAvailable: true }))} onAction={onAction} busy />);
    const copy = screen.getByRole("button", { name: "Copy text of retained draft" });
    expect(copy).toHaveAttribute("aria-disabled", "true");
    fireEvent.click(copy);
    expect(onAction).not.toHaveBeenCalled();
  });

  it("shows a saving notice only after the 600ms delay", () => {
    vi.useFakeTimers();
    render(<DraftNotice notice={draftNoticeView(candidateOf("saving", 1))} onAction={vi.fn()} />);
    expect(screen.queryByRole("status")).toBeNull();
    act(() => { vi.advanceTimersByTime(599); });
    expect(screen.queryByRole("status")).toBeNull();
    act(() => { vi.advanceTimersByTime(1); });
    expect(screen.getByRole("status")).toHaveAttribute("data-kind", "saving");
  });

  it("shows a loading notice only after the 600ms delay", () => {
    vi.useFakeTimers();
    render(<DraftNotice notice={draftNoticeView(candidateOf("loading", 1))} onAction={vi.fn()} />);
    act(() => { vi.advanceTimersByTime(599); });
    expect(screen.queryByRole("status")).toBeNull();
    act(() => { vi.advanceTimersByTime(1); });
    expect(screen.getByRole("status")).toHaveAttribute("data-kind", "loading");
  });

  it("restarts the pending delay when the pending notice is interrupted and returns", () => {
    vi.useFakeTimers();
    const pending = draftNoticeView(candidateOf("saving", 1));
    const { rerender } = render(<DraftNotice notice={pending} onAction={vi.fn()} />);
    act(() => { vi.advanceTimersByTime(400); });
    rerender(<DraftNotice notice={undefined} onAction={vi.fn()} />);
    rerender(<DraftNotice notice={pending} onAction={vi.fn()} />);
    act(() => { vi.advanceTimersByTime(599); });
    expect(screen.queryByRole("status")).toBeNull();
    act(() => { vi.advanceTimersByTime(1); });
    expect(screen.getByRole("status")).toBeVisible();
  });

  it("restarts the pending delay when saving changes to loading", () => {
    vi.useFakeTimers();
    const { rerender } = render(<DraftNotice notice={draftNoticeView(candidateOf("saving", 1))} onAction={vi.fn()} />);
    act(() => { vi.advanceTimersByTime(400); });
    rerender(<DraftNotice notice={draftNoticeView(candidateOf("loading", 2))} onAction={vi.fn()} />);
    expect(screen.queryByRole("status")).toBeNull();
    act(() => { vi.advanceTimersByTime(599); });
    expect(screen.queryByRole("status")).toBeNull();
    act(() => { vi.advanceTimersByTime(1); });
    expect(screen.getByRole("status")).toHaveAttribute("data-kind", "loading");
  });

  it("shows a failed send reason immediately", () => {
    render(<DraftNotice notice={draftNoticeView({ kind: "send-failed", sequence: 1, detail: "No route" })} onAction={vi.fn()} />);
    expect(screen.getByRole("alert")).toHaveTextContent("No route");
  });
});
