import "@testing-library/jest-dom/vitest";
import { act, cleanup, fireEvent, render, screen } from "@testing-library/react";
import { describe, expect, it, vi } from "vitest";
import { DraftSyncStatus } from "./DraftSyncStatus";

const draft = {
  id: "draft-1", conversationId: "conversation-1", text: "local caption", recipientIds: ["+12025550123"], attachmentIds: ["local-image"], revision: "1",
  sync: {
    state: "synced" as const, expiresAt: new Date(Date.now() + 12 * 60 * 60 * 1000).toISOString(), sendRequiresConnection: true,
    notice: "restored-after-expiry" as const,
    conflicts: [{ branchId: "branch-2", text: "other caption", attachmentIds: ["other-image"], recipientIds: ["+12025550124"] }],
  },
};

describe("DraftSyncStatus", () => {
  it("updates expiry when the minute timer crosses the warning threshold", () => {
    vi.useFakeTimers();
    vi.setSystemTime(new Date("2026-01-01T00:00:00Z"));
    render(<DraftSyncStatus draft={{ ...draft, sync: { ...draft.sync, expiresAt: "2026-01-02T00:00:30Z", conflicts: [] } }} local={draft} offline={false} localSaveState="idle" attachments={{}} onResolveConflict={vi.fn()} onAcknowledgeNotice={vi.fn()} />);
    expect(screen.getByText(/Expires Jan/)).toBeInTheDocument();
    act(() => vi.advanceTimersByTime(60_000));
    expect(screen.getByText("Expires within 24 hours")).toBeInTheDocument();
    cleanup();
    vi.useRealTimers();
  });

  it("shows sync recovery, expiry, offline sending, and conflict resolution", () => {
    const resolve = vi.fn();
    const acknowledge = vi.fn();
    render(<DraftSyncStatus draft={draft} local={draft} offline localSaveState="idle" attachments={{
      "local-image": { id: "local-image", name: "local.png", state: "ready" },
      "other-image": { id: "other-image", name: "other.png", state: "ready" },
    }} onResolveConflict={resolve} onAcknowledgeNotice={acknowledge} />);

    expect(screen.getByText("Draft synced")).toBeInTheDocument();
    expect(screen.getByText("Reconnect to send this synced draft.")).toBeInTheDocument();
    expect(screen.getByText("Expires within 24 hours")).toBeInTheDocument();
    expect(screen.getByText("Offline changes restored as a new draft.")).toBeInTheDocument();
    fireEvent.click(screen.getByText("Dismiss"));
    fireEvent.click(screen.getByText("Review versions"));
    expect(screen.getByText("other caption")).toBeInTheDocument();
    fireEvent.click(screen.getByText("Choose other version"));
    expect(resolve).toHaveBeenCalledWith("branch-2");
    expect(acknowledge).toHaveBeenCalledOnce();
  });
});
