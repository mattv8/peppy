import "@testing-library/jest-dom/vitest";
import { act, cleanup, fireEvent, render, screen } from "@testing-library/react";
import type { ComponentProps } from "react";
import { afterEach, describe, expect, it, vi } from "vitest";
import type { Draft } from "@peppy/desktop-ui/bridge-contracts";
import { DraftSyncStatus, draftSyncNeedsAttention } from "./DraftSyncStatus";

const now = Date.parse("2026-01-01T00:00:00Z");
const local = {
  text: "local caption",
  recipientIds: ["+12025550123"],
  attachmentIds: ["local-image"],
};

function draft(overrides: Partial<Draft["sync"]> = {}): Draft {
  return {
    id: "draft-1",
    conversationId: "conversation-1",
    text: local.text,
    recipientIds: local.recipientIds,
    attachmentIds: local.attachmentIds,
    revision: "1",
    sync: {
      state: "synced",
      expiresAt: null,
      sendRequiresConnection: false,
      notice: null,
      conflicts: [],
      ...overrides,
    },
  };
}

const DAY_MS = 24 * 60 * 60 * 1000;
const conflict = { branchId: "branch-2", text: "other", recipientIds: [], attachmentIds: [] };

type StatusProps = Partial<ComponentProps<typeof DraftSyncStatus>>;

function statusElement(props: StatusProps = {}) {
  return <div id="composer-field"><textarea id="composer-textarea" /><DraftSyncStatus
    draft={draft()}
    local={local}
    attachments={{}}
    offline={false}
    localSaveState="idle"
    onResolveConflict={vi.fn().mockResolvedValue({ ok: true })}
    onAcknowledgeNotice={vi.fn()}
    {...props}
  /></div>;
}

function renderStatus(props: StatusProps = {}) {
  return render(statusElement(props));
}

afterEach(() => cleanup());

describe("draftSyncNeedsAttention", () => {
  it.each([
    ["has no draft", undefined, false, "idle", false],
    ["is synced", draft(), false, "idle", false],
    ["is syncing", draft({ state: "syncing" }), false, "idle", false],
    ["is saving remotely", draft({ state: "saving" }), false, "idle", false],
    ["is saved locally", draft({ state: "saved-local" }), false, "idle", false],
    ["is local only", draft({ state: "local-only" }), false, "idle", false],
    ["has a failed local save", draft(), false, "failed", true],
    ["has conflicts", draft({ conflicts: [{ branchId: "branch-2", text: "other", recipientIds: [], attachmentIds: [] }] }), false, "idle", true],
    ["has a failed sync", draft({ state: "failed" }), false, "idle", true],
    ["has a paused sync", draft({ state: "paused" }), false, "idle", false],
    ["has a paused sync while offline", draft({ state: "paused" }), true, "idle", false],
    ["requires a connection while offline", draft({ sendRequiresConnection: true }), true, "idle", true],
    ["expires within one day", draft({ expiresAt: new Date(now + 60_000).toISOString() }), false, "idle", true],
    ["has expired", draft({ expiresAt: new Date(now - 60_000).toISOString() }), false, "idle", true],
    ["has a notice", draft({ notice: "kept-after-sent" }), false, "idle", true],
    ["has an other-device update", draft({ notice: "updated-from-other-device" }), false, "idle", false],
  ] as const)("returns %s", (_name, statusDraft, offline, localSaveState, expected) => {
    expect(draftSyncNeedsAttention({ draft: statusDraft, offline, localSaveState, now })).toBe(expected);
  });
});

describe("DraftSyncStatus", () => {
  it("hides the status row for an ordinary synced draft", () => {
    render(<DraftSyncStatus draft={draft()} local={local} attachments={{}} offline={false} localSaveState="idle" onResolveConflict={vi.fn().mockResolvedValue({ ok: true })} onAcknowledgeNotice={vi.fn()} />);

    expect(document.querySelector("#draft-sync-status")).not.toBeInTheDocument();
  });

  it("opens conflict review from the attention row", () => {
    render(<div id="composer-field"><textarea id="composer-textarea" /><DraftSyncStatus draft={draft({ conflicts: [{ branchId: "branch-2", text: "other", recipientIds: [], attachmentIds: [] }] })} local={local} attachments={{}} offline={false} localSaveState="idle" onResolveConflict={vi.fn().mockResolvedValue({ ok: true })} onAcknowledgeNotice={vi.fn()} /></div>);

    fireEvent.click(screen.getByRole("button", { name: "Review" }));

    expect(screen.getByRole("dialog", { name: "Review draft versions" })).toBeInTheDocument();
  });

  it.each([
    ["local save failure", draft({ state: "failed", conflicts: [conflict] }), true, "failed", "Draft not saved on this device. Keep this window open."],
    ["conflicts", draft({ state: "failed", conflicts: [conflict] }), true, "idle", "Draft versions need review"],
    ["sync failure", draft({ state: "failed", sendRequiresConnection: true }), true, "idle", "Draft sync failed."],
    ["offline send", draft({ sendRequiresConnection: true }), true, "idle", "Reconnect to check this draft before sending."],
    ["paused sync that blocks an offline send", draft({ state: "paused", sendRequiresConnection: true }), true, "idle", "Reconnect to check this draft before sending."],
  ] as const)("shows %s as the primary reason", (_name, statusDraft, offline, localSaveState, message) => {
    renderStatus({ draft: statusDraft, offline, localSaveState });

    expect(document.querySelector("[data-draft-sync-message]")).toHaveTextContent(message);
    expect(document.querySelector("[data-draft-sync-message]")).toHaveAttribute("data-inline-status-message");
    expect(document.querySelector("[data-draft-sync-message]")).toHaveAttribute("title", message);
  });

  it("stays silent for a paused sync that does not block sending", () => {
    renderStatus({ draft: draft({ state: "paused" }) });

    expect(document.querySelector("#draft-sync-status")).not.toBeInTheDocument();
  });

  it.each([
    ["restored-after-expiry", "Restored your offline changes."],
    ["kept-after-sent", "Kept your offline edits as a new draft."],
  ] as const)("shows the %s notice", (notice, text) => {
    renderStatus({ draft: draft({ notice }) });

    expect(document.querySelector("[data-draft-sync-notice]")).toHaveTextContent(text);
    expect(document.querySelector("[data-draft-sync-notice] > [data-inline-status-message]")).toHaveAttribute("title", text);
    expect(screen.queryByText("New 30-day period")).not.toBeInTheDocument();
  });

  it("dismisses a sync notice", () => {
    const onAcknowledgeNotice = vi.fn();
    renderStatus({ draft: draft({ notice: "kept-after-sent" }), onAcknowledgeNotice });

    fireEvent.click(screen.getByRole("button", { name: "Dismiss" }));

    expect(onAcknowledgeNotice).toHaveBeenCalledOnce();
  });

  it("shows an other-device update temporarily before acknowledging it", () => {
    vi.useFakeTimers();
    const onAcknowledgeNotice = vi.fn();
    try {
      renderStatus({ draft: draft({ notice: "updated-from-other-device" }), onAcknowledgeNotice });

      expect(screen.getByRole("status")).toHaveTextContent("Updated from your other device.");
      expect(document.querySelector("#draft-sync-status")).toContainElement(screen.getByRole("status"));
      expect(document.querySelector("[data-draft-sync-notice] > [data-inline-status-message]")).toHaveAttribute("title", "Updated from your other device.");

      act(() => { vi.advanceTimersByTime(4_000); });

      expect(onAcknowledgeNotice).toHaveBeenCalledOnce();
    } finally {
      vi.useRealTimers();
    }
  });

  it("closes review when the draft changes identity", () => {
    const { rerender } = renderStatus({ draft: draft({ conflicts: [conflict] }) });
    fireEvent.click(screen.getByRole("button", { name: "Review" }));

    rerender(statusElement({ draft: { ...draft({ conflicts: [conflict] }), id: "draft-2" } }));

    expect(screen.queryByRole("dialog", { name: "Review draft versions" })).not.toBeInTheDocument();
  });

  it("closes review and returns focus to the composer when conflicts disappear", () => {
    const { rerender } = renderStatus({ draft: draft({ conflicts: [conflict] }) });
    fireEvent.click(screen.getByRole("button", { name: "Review" }));

    rerender(statusElement({ draft: draft() }));

    expect(screen.queryByRole("dialog", { name: "Review draft versions" })).not.toBeInTheDocument();
    expect(document.getElementById("composer-textarea")).toHaveFocus();
  });

  it("shows expiry help when the 24-hour warning begins", () => {
    vi.useFakeTimers({ now, toFake: ["Date", "setTimeout", "clearTimeout"] });
    try {
      renderStatus({ draft: draft({ expiresAt: new Date(now + DAY_MS + 60_000).toISOString() }) });
      expect(document.querySelector("#draft-sync-status")).not.toBeInTheDocument();

      act(() => { vi.advanceTimersByTime(60_001); });

      expect(screen.getByText("Expires within 24 hours")).toBeInTheDocument();
    } finally {
      vi.useRealTimers();
    }
  });

  it("keeps the primary attention reason ahead of additional facts", () => {
    render(<DraftSyncStatus draft={draft({
      state: "failed",
      sendRequiresConnection: true,
      expiresAt: new Date(now + 60_000).toISOString(),
      notice: "restored-after-expiry",
    })} local={local} attachments={{}} offline localSaveState="failed" onResolveConflict={vi.fn().mockResolvedValue({ ok: true })} onAcknowledgeNotice={vi.fn()} />);

    expect(document.querySelector("[data-draft-sync-message]")).toHaveTextContent("Draft not saved on this device. Keep this window open.");
    expect(screen.getByRole("button", { name: "Dismiss" })).toBeInTheDocument();
  });
});
