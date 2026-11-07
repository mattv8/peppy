import "@testing-library/jest-dom/vitest";
import { act, cleanup, fireEvent, render, screen } from "@testing-library/react";
import { createRef } from "react";
import { afterEach, describe, expect, it, vi } from "vitest";
import type { Attachment } from "@peppy/desktop-ui";
import type { DraftConflictView } from "@peppy/desktop-ui/bridge-contracts";
import { DraftConflictReviewDialog } from "./DraftConflictReviewDialog";

const local = {
  text: "This device caption",
  recipientIds: ["+12025550123"],
  attachmentIds: ["local-image"],
};

const conflicts: DraftConflictView[] = [{
  branchId: "branch-2",
  text: "Other version caption",
  recipientIds: ["+12025550124"],
  attachmentIds: ["other-image"],
}];

const attachments: Record<string, Attachment> = {
  "local-image": { id: "local-image", name: "local.png", state: "ready" },
  "other-image": { id: "other-image", name: "other.png", state: "ready" },
};

function renderDialog(overrides: Partial<React.ComponentProps<typeof DraftConflictReviewDialog>> = {}) {
  const onResolve = overrides.onResolve ?? vi.fn().mockResolvedValue({ ok: true });
  const onClose = overrides.onClose ?? vi.fn();
  const result = render(<DraftConflictReviewDialog
    draftId="draft-1"
    local={local}
    conflicts={conflicts}
    attachments={attachments}
    opener={createRef<HTMLElement>()}
    focusFallback={createRef<HTMLElement>()}
    onResolve={onResolve}
    onClose={onClose}
    {...overrides}
  />);
  return { ...result, onResolve, onClose };
}

afterEach(() => cleanup());

describe("DraftConflictReviewDialog", () => {
  it("cancelling never resolves a version", () => {
    const { onResolve, onClose } = renderDialog();

    fireEvent.click(screen.getByRole("button", { name: "Cancel" }));

    expect(onResolve).not.toHaveBeenCalled();
    expect(onClose).toHaveBeenCalledOnce();
  });

  it("submits null for this device", async () => {
    const { onResolve } = renderDialog();

    fireEvent.click(screen.getByRole("radio", { name: /This device/ }));
    fireEvent.click(screen.getByRole("button", { name: "Use selected version" }));

    await act(async () => undefined);
    expect(onResolve).toHaveBeenCalledExactlyOnceWith(null);
  });

  it("submits the selected branch id", async () => {
    const { onResolve } = renderDialog();

    fireEvent.click(screen.getByRole("radio", { name: /Other version/ }));
    fireEvent.click(screen.getByRole("button", { name: "Use selected version" }));

    await act(async () => undefined);
    expect(onResolve).toHaveBeenCalledExactlyOnceWith("branch-2");
  });

  it("keeps the dialog open and resets selection after a failed resolution", async () => {
    const onResolve = vi.fn().mockResolvedValue({ ok: false, message: "Could not apply version" });
    renderDialog({ onResolve });

    fireEvent.click(screen.getByRole("radio", { name: /Other version/ }));
    fireEvent.click(screen.getByRole("button", { name: "Use selected version" }));

    expect(await screen.findByRole("alert")).toHaveTextContent("Could not apply version");
    expect(screen.getByRole("radio", { name: /Other version/ })).not.toBeChecked();
    expect(screen.getByRole("button", { name: "Use selected version" })).toBeDisabled();
  });

  it("recovers from a rejected resolution promise", async () => {
    renderDialog({ onResolve: vi.fn().mockRejectedValue(new Error("Native resolver failed")) });

    fireEvent.click(screen.getByRole("radio", { name: /Other version/ }));
    fireEvent.click(screen.getByRole("button", { name: "Use selected version" }));

    expect(await screen.findByRole("alert")).toHaveTextContent("Native resolver failed");
    expect(screen.getByRole("button", { name: "Use selected version" })).toBeDisabled();
  });

  it("clears selection when reviewed content becomes stale", () => {
    const { rerender } = renderDialog();

    fireEvent.click(screen.getByRole("radio", { name: /This device/ }));
    rerender(<DraftConflictReviewDialog
      draftId="draft-1"
      local={{ ...local, text: "Updated local caption" }}
      conflicts={conflicts}
      attachments={attachments}
      opener={createRef<HTMLElement>()}
      focusFallback={createRef<HTMLElement>()}
      onResolve={vi.fn()}
      onClose={vi.fn()}
    />);

    expect(screen.getByRole("radio", { name: /This device/ })).not.toBeChecked();
    expect(screen.getByRole("status")).toHaveTextContent(/versions changed/i);
  });

  it("offers only Close when conflicts no longer need review", () => {
    renderDialog({ conflicts: [] });

    expect(screen.getByRole("button", { name: "Close" })).toBeEnabled();
    expect(screen.queryByRole("radio")).not.toBeInTheDocument();
    expect(screen.queryByRole("button", { name: "Use selected version" })).not.toBeInTheDocument();
  });

  it("contains Escape from window handlers", () => {
    const closeComposer = vi.fn();
    window.addEventListener("keydown", closeComposer);
    renderDialog();

    fireEvent.keyDown(screen.getByRole("dialog", { name: "Review draft versions" }), { key: "Escape" });

    expect(closeComposer).not.toHaveBeenCalled();
    window.removeEventListener("keydown", closeComposer);
  });

  it("dismisses on Escape without resolving", () => {
    const { onResolve, onClose } = renderDialog();

    fireEvent.keyDown(screen.getByRole("dialog", { name: "Review draft versions" }), { key: "Escape" });

    expect(onClose).toHaveBeenCalled();
    expect(onResolve).not.toHaveBeenCalled();
  });

  it("contains Escape from window handlers while applying a version", async () => {
    const closeComposer = vi.fn();
    window.addEventListener("keydown", closeComposer);
    renderDialog({ onResolve: vi.fn(() => new Promise<never>(() => undefined)) });

    fireEvent.click(screen.getByRole("radio", { name: /Other version/ }));
    fireEvent.click(screen.getByRole("button", { name: "Use selected version" }));
    await screen.findByText("Applying selected version…");
    fireEvent.keyDown(screen.getByRole("dialog", { name: "Review draft versions" }), { key: "Escape" });

    expect(closeComposer).not.toHaveBeenCalled();
    window.removeEventListener("keydown", closeComposer);
  });

  it("disables confirmation until a version is selected", () => {
    renderDialog();

    expect(screen.getByRole("button", { name: "Use selected version" })).toBeDisabled();
  });
});
