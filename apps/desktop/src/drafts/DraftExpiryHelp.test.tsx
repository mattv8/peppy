import "@testing-library/jest-dom/vitest";
import { cleanup, fireEvent, render, screen } from "@testing-library/react";
import { afterEach, describe, expect, it, vi } from "vitest";
import { DraftExpiryHelp } from "./DraftExpiryHelp";

afterEach(() => cleanup());

describe("DraftExpiryHelp", () => {
  it("opens only on click of the info button", () => {
    render(<DraftExpiryHelp draftId="draft-1" label="Expires within 24 hours" nearExpiry />);

    expect(screen.queryByRole("dialog", { name: "Draft expiry" })).not.toBeInTheDocument();
    fireEvent.click(screen.getByRole("button", { name: "About draft expiry" }));
    expect(screen.getByRole("dialog", { name: "Draft expiry" })).toBeInTheDocument();
  });

  it("returns focus to the trigger when Close is pressed", () => {
    render(<DraftExpiryHelp draftId="draft-1" label="Expires within 24 hours" nearExpiry />);

    const info = screen.getByRole("button", { name: "About draft expiry" });
    fireEvent.click(info);
    fireEvent.click(screen.getByRole("button", { name: "Close" }));
    expect(screen.queryByRole("dialog", { name: "Draft expiry" })).not.toBeInTheDocument();
    expect(document.activeElement).toBe(info);
  });

  it("closes the dialog when the draft identity changes", () => {
    const { rerender } = render(<DraftExpiryHelp draftId="draft-1" label="Expires within 24 hours" nearExpiry />);
    fireEvent.click(screen.getByRole("button", { name: "About draft expiry" }));
    expect(screen.getByRole("dialog", { name: "Draft expiry" })).toBeInTheDocument();

    rerender(<DraftExpiryHelp draftId="draft-2" label="Expires within 24 hours" nearExpiry />);
    expect(screen.queryByRole("dialog", { name: "Draft expiry" })).not.toBeInTheDocument();
  });

  it("contains Escape pressed inside the open help so window listeners do not receive it", () => {
    const closeComposer = vi.fn();
    window.addEventListener("keydown", closeComposer);
    try {
      render(<DraftExpiryHelp draftId="draft-1" label="Expires within 24 hours" nearExpiry />);
      fireEvent.click(screen.getByRole("button", { name: "About draft expiry" }));
      fireEvent.keyDown(screen.getByRole("dialog", { name: "Draft expiry" }), { key: "Escape" });

      expect(closeComposer).not.toHaveBeenCalled();
      expect(screen.queryByRole("dialog", { name: "Draft expiry" })).not.toBeInTheDocument();
      fireEvent.keyDown(window, { key: "Escape" });
      expect(closeComposer).toHaveBeenCalledOnce();
    } finally {
      window.removeEventListener("keydown", closeComposer);
    }
  });
});
