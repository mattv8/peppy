import "@testing-library/jest-dom/vitest";
import { act, cleanup, fireEvent, render, screen } from "@testing-library/react";
import { afterEach, describe, expect, it, vi } from "vitest";
import type { Draft } from "@peppy/desktop-ui/bridge-contracts";
import { DraftSyncStatus } from "./DraftSyncStatus";

const draft = {
  id: "draft-1", conversationId: "conversation-1", text: "local caption", recipientIds: ["+12025550123"], attachmentIds: ["local-image"], revision: "1",
  sync: {
    state: "synced" as const, expiresAt: new Date(Date.now() + 12 * 60 * 60 * 1000).toISOString(), sendRequiresConnection: true,
    notice: "restored-after-expiry" as const,
    conflicts: [{ branchId: "branch-2", text: "other caption", attachmentIds: ["other-image"], recipientIds: ["+12025550124"] }],
  },
};

afterEach(() => cleanup());

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

  it("opens expiry policy help deliberately and returns focus to its opener", () => {
    render(<DraftSyncStatus draft={draft} local={draft} offline={false} localSaveState="idle" attachments={{}} onResolveConflict={vi.fn()} onAcknowledgeNotice={vi.fn()} />);

    const info = screen.getByRole("button", { name: "About draft expiry" });
    fireEvent.click(info);
    expect(screen.getByRole("dialog", { name: "Draft expiry" })).toBeInTheDocument();
    fireEvent.click(screen.getByRole("button", { name: "Close" }));
    expect(document.activeElement).toBe(info);
  });

  it("closes expiry help when the draft identity changes", () => {
    const { rerender } = render(<DraftSyncStatus draft={draft} local={draft} offline={false} localSaveState="idle" attachments={{}} onResolveConflict={vi.fn()} onAcknowledgeNotice={vi.fn()} />);
    fireEvent.click(screen.getByRole("button", { name: "About draft expiry" }));
    expect(screen.getByRole("dialog", { name: "Draft expiry" })).toBeInTheDocument();

    rerender(<DraftSyncStatus draft={{ ...draft, id: "draft-2" }} local={draft} offline={false} localSaveState="idle" attachments={{}} onResolveConflict={vi.fn()} onAcknowledgeNotice={vi.fn()} />);
    expect(screen.queryByRole("dialog", { name: "Draft expiry" })).not.toBeInTheDocument();
  });

  it("does not reopen expiry help after its context disappears and returns", () => {
    const { rerender } = render(<DraftSyncStatus draft={draft} local={draft} offline={false} localSaveState="idle" attachments={{}} onResolveConflict={vi.fn()} onAcknowledgeNotice={vi.fn()} />);
    fireEvent.click(screen.getByRole("button", { name: "About draft expiry" }));
    rerender(<DraftSyncStatus draft={{ ...draft, sync: { ...draft.sync, expiresAt: null } }} local={draft} offline={false} localSaveState="idle" attachments={{}} onResolveConflict={vi.fn()} onAcknowledgeNotice={vi.fn()} />);
    rerender(<DraftSyncStatus draft={draft} local={draft} offline={false} localSaveState="idle" attachments={{}} onResolveConflict={vi.fn()} onAcknowledgeNotice={vi.fn()} />);

    expect(screen.queryByRole("dialog", { name: "Draft expiry" })).not.toBeInTheDocument();
  });

  it("keeps expiry help open through same-draft deadline updates", () => {
    const { rerender } = render(<DraftSyncStatus draft={draft} local={draft} offline={false} localSaveState="idle" attachments={{}} onResolveConflict={vi.fn()} onAcknowledgeNotice={vi.fn()} />);
    fireEvent.click(screen.getByRole("button", { name: "About draft expiry" }));
    rerender(<DraftSyncStatus draft={{ ...draft, sync: { ...draft.sync, expiresAt: new Date(Date.now() + 24 * 60 * 60 * 1000).toISOString() } }} local={draft} offline={false} localSaveState="idle" attachments={{}} onResolveConflict={vi.fn()} onAcknowledgeNotice={vi.fn()} />);

    expect(screen.getByRole("dialog", { name: "Draft expiry" })).toBeInTheDocument();
  });

  it("contains Escape while expiry help is open", () => {
    const closeComposer = vi.fn();
    window.addEventListener("keydown", closeComposer);
    render(<DraftSyncStatus draft={draft} local={draft} offline={false} localSaveState="idle" attachments={{}} onResolveConflict={vi.fn()} onAcknowledgeNotice={vi.fn()} />);
    fireEvent.click(screen.getByRole("button", { name: "About draft expiry" }));
    fireEvent.keyDown(screen.getByRole("dialog", { name: "Draft expiry" }), { key: "Escape" });

    expect(closeComposer).not.toHaveBeenCalled();
    expect(screen.queryByRole("dialog", { name: "Draft expiry" })).not.toBeInTheDocument();
    fireEvent.keyDown(window, { key: "Escape" });
    expect(closeComposer).toHaveBeenCalledOnce();
    window.removeEventListener("keydown", closeComposer);
  });

  it("returns focus to the composer textarea when expiry context disappears", () => {
    const renderStatus = (statusDraft: Draft) => <div id="composer-field"><textarea id="composer-textarea" /><DraftSyncStatus draft={statusDraft} local={draft} offline={false} localSaveState="idle" attachments={{}} onResolveConflict={vi.fn()} onAcknowledgeNotice={vi.fn()} /></div>;
    const { rerender } = render(renderStatus(draft));
    fireEvent.click(screen.getByRole("button", { name: "About draft expiry" }));
    rerender(renderStatus({ ...draft, sync: { ...draft.sync, expiresAt: null } }));

    expect(document.activeElement).toBe(document.getElementById("composer-textarea"));
  });

  it("keeps local and remote save failures distinct", () => {
    const { rerender } = render(<DraftSyncStatus draft={{ ...draft, sync: { ...draft.sync, state: "failed", conflicts: [] } }} local={draft} offline={false} localSaveState="idle" attachments={{}} onResolveConflict={vi.fn()} onAcknowledgeNotice={vi.fn()} />);
    expect(screen.getByText("Draft sync failed.")).toBeInTheDocument();

    rerender(<DraftSyncStatus draft={draft} local={draft} offline={false} localSaveState="failed" attachments={{}} onResolveConflict={vi.fn()} onAcknowledgeNotice={vi.fn()} />);
    expect(screen.getByText("Draft not saved on this device. Keep this window open.")).toBeInTheDocument();
  });

  it("keeps conflict replacement consequences beside disabled resolving choices", () => {
    render(<DraftSyncStatus draft={draft} local={draft} offline={false} localSaveState="idle" resolving attachments={{}} onResolveConflict={vi.fn()} onAcknowledgeNotice={vi.fn()} />);
    fireEvent.click(screen.getByText("Review versions"));

    const choices = screen.getAllByRole("button", { name: /Keep this version|Choose other version/ });
    expect(choices).toHaveLength(2);
    expect(choices[0]).toHaveAttribute("aria-describedby");
    expect(choices[0]).toBeDisabled();
    expect(screen.getByRole("status")).toBeInTheDocument();
  });
});
