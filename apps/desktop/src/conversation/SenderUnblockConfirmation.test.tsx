import "@testing-library/jest-dom/vitest";
import { cleanup, fireEvent, render, screen, waitFor, within } from "@testing-library/react";
import { afterEach, describe, expect, it, vi } from "vitest";
import { SenderUnblockConfirmation } from "./SenderUnblockConfirmation";

describe("SenderUnblockConfirmation", () => {
  afterEach(() => { cleanup(); vi.restoreAllMocks(); });

  it("shows the allowed unblock confirmation and focuses Unblock", () => {
    const trigger = document.createElement("button");
    document.body.append(trigger);
    render(<SenderUnblockConfirmation name="Ada" canUnblock trigger={trigger} pending={false} onConfirm={vi.fn()} onCancel={vi.fn()} />);
    const dialog = screen.getByRole("dialog", { name: "Unblock Ada on your phone?" });
    expect(dialog).toHaveTextContent("Unblock Ada on your phone?");
    expect(dialog).toHaveTextContent("Your phone removes this number from its blocked list.");
    expect(within(dialog).getByRole("button", { name: "Unblock" })).toHaveFocus();
    trigger.remove();
  });

  it("shows Close only when the phone cannot unblock", () => {
    const trigger = document.createElement("button");
    document.body.append(trigger);
    render(<SenderUnblockConfirmation name="Ada" canUnblock={false} trigger={trigger} pending={false} onConfirm={vi.fn()} onCancel={vi.fn()} />);
    const dialog = screen.getByRole("dialog");
    expect(dialog).toHaveTextContent("Your phone can't unblock Ada yet.");
    expect(dialog).toHaveAccessibleDescription("Turn on Advanced features in Peppy on your phone, or unblock this number in your messaging app.");
    expect(screen.getByRole("button", { name: "Close" })).toHaveFocus();
    expect(screen.queryByRole("button", { name: "Unblock" })).not.toBeInTheDocument();
    trigger.remove();
  });

  it("cancels on Escape and outside click, restoring the trigger after Escape", async () => {
    const trigger = document.createElement("button");
    document.body.append(trigger);
    const onCancel = vi.fn();
    render(<><button>Outside</button><SenderUnblockConfirmation name="Ada" canUnblock trigger={trigger} pending={false} onConfirm={vi.fn()} onCancel={onCancel} /></>);
    fireEvent.keyDown(document, { key: "Escape" });
    await waitFor(() => expect(trigger).toHaveFocus());
    fireEvent.pointerDown(screen.getByRole("button", { name: "Outside" }));
    expect(onCancel).toHaveBeenCalledTimes(2);
    trigger.remove();
  });

  it("restores trigger focus when Close is used", async () => {
    const trigger = document.createElement("button");
    document.body.append(trigger);
    const onCancel = vi.fn();
    render(<SenderUnblockConfirmation name="Ada" canUnblock={false} trigger={trigger} pending={false} onConfirm={vi.fn()} onCancel={onCancel} />);
    fireEvent.click(screen.getByRole("button", { name: "Close" }));
    await waitFor(() => expect(trigger).toHaveFocus());
    expect(onCancel).toHaveBeenCalledOnce();
    trigger.remove();
  });
});
