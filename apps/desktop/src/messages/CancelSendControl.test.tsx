import "@testing-library/jest-dom/vitest";
import { cleanup, fireEvent, render, screen } from "@testing-library/react";
import { afterEach, describe, expect, it, vi } from "vitest";
import { CancelSendControl, SendCancellationPending } from "./CancelSendControl";

afterEach(() => {
  cleanup();
  vi.useRealTimers();
});

describe("CancelSendControl", () => {
  it("names the destructive action and binds its hook to the message identity", () => {
    const { rerender } = render(<CancelSendControl messageId="message-a" onCancel={() => {}} />);
    const button = screen.getByRole("button", { name: "Cancel send and remove message" });
    expect(button).toHaveAttribute("data-message-cancel", "message-a");
    expect(button).toHaveAttribute("type", "button");
    expect(button.querySelector("svg")).toHaveAttribute("aria-hidden", "true");

    rerender(<CancelSendControl messageId="message-b" onCancel={() => {}} />);
    expect(button).toHaveAttribute("data-message-cancel", "message-b");
  });

  it("requests cancellation on click without submitting a containing form", () => {
    const onCancel = vi.fn();
    const onSubmit = vi.fn((event: React.FormEvent) => event.preventDefault());
    render(<form data-test-cancel-form onSubmit={onSubmit}>
      <CancelSendControl messageId="message-a" onCancel={onCancel} />
    </form>);

    fireEvent.click(screen.getByRole("button"));
    expect(onCancel).toHaveBeenCalledTimes(1);
    expect(onSubmit).not.toHaveBeenCalled();
  });

  it("accepts focus and keyboard-generated activation through a native button", () => {
    const onCancel = vi.fn();
    render(<CancelSendControl messageId="message-a" onCancel={onCancel} />);
    const button = screen.getByRole("button");

    expect(button.tagName).toBe("BUTTON");
    expect(button.tabIndex).toBe(0);
    button.focus();
    expect(button).toHaveFocus();
    fireEvent.click(button, { detail: 0 });
    expect(onCancel).toHaveBeenCalledTimes(1);
  });

  it("blocks the action while disabled and allows it again when enabled", () => {
    const onCancel = vi.fn();
    const { rerender } = render(<CancelSendControl messageId="message-a" onCancel={onCancel} disabled />);
    const button = screen.getByRole("button");
    expect(button).toBeDisabled();
    fireEvent.click(button);
    expect(onCancel).not.toHaveBeenCalled();

    rerender(<CancelSendControl messageId="message-a" onCancel={onCancel} disabled={false} />);
    expect(button).toBeEnabled();
    fireEvent.click(button);
    expect(onCancel).toHaveBeenCalledTimes(1);
  });
});

describe("SendCancellationPending", () => {
  it("provides an identity-bound focusable live status for focus handoff", () => {
    render(<SendCancellationPending messageId="message-a" />);
    const status = screen.getByRole("status");
    expect(status).toHaveAttribute("data-message-cancellation", "message-a");
    expect(status).toHaveAttribute("aria-atomic", "true");
    expect(status).toHaveAttribute("tabindex", "-1");
    status.focus();
    expect(status).toHaveFocus();
    expect(status).toHaveTextContent("Cancelling…");
    expect(screen.queryByRole("button")).not.toBeInTheDocument();
  });

  it("keeps pending copy visible while waiting for the phone, without a timeout", () => {
    vi.useFakeTimers();
    const { rerender } = render(<SendCancellationPending messageId="message-a" />);
    rerender(<SendCancellationPending messageId="message-a" waitingForPhone />);
    vi.advanceTimersByTime(60_000);

    const status = screen.getByRole("status");
    expect(status).toHaveTextContent("Cancelling…");
    expect(status).toHaveTextContent(/waiting for phone/i);
    expect(status).toBeVisible();
  });
});
