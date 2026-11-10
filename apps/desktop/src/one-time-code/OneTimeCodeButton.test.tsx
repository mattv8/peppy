import "@testing-library/jest-dom/vitest";
import { act, cleanup, fireEvent, render, screen, waitFor } from "@testing-library/react";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import type { OneTimeCodeTarget } from "@peppy/desktop-ui/bridge-contracts";
import { OneTimeCodeButton } from "./OneTimeCodeButton";

type CopyHandler = (target: OneTimeCodeTarget, code: string) => Promise<void>;

const messageTarget = { kind: "message" as const, messageId: "message-1" };
const notificationTarget = { kind: "notification" as const, target: { sourceDeviceId: "phone-a", notificationKey: "chat-1", lifetime: "one" } };

const messageButton = (onCopy: CopyHandler, code = "482913") => (
  <OneTimeCodeButton variant="message" hookId="message-1" target={messageTarget} code={code} onCopy={onCopy} />
);

beforeEach(() => {
  vi.useFakeTimers({ shouldAdvanceTime: true });
});

afterEach(() => {
  cleanup();
  vi.useRealTimers();
  vi.restoreAllMocks();
});

describe("OneTimeCodeButton", () => {
  it("names the button with the code and hides the visible label from assistive technology", () => {
    render(messageButton(vi.fn()));

    const button = screen.getByRole("button", { name: /^Copy code 482913$/ });
    expect(button).toHaveAttribute("type", "button");
    expect(button).toHaveAttribute("data-one-time-code-button", "message-1");
    expect(screen.getByText("Copy code")).toHaveAttribute("aria-hidden", "true");
  });

  it("includes the app name in the accessible name for notifications", () => {
    render(<OneTimeCodeButton variant="notification" hookId="notif-1" target={notificationTarget} code="839201" contextName="Chat" onCopy={vi.fn()} />);

    expect(screen.getByRole("button", { name: "Copy code 839201 from Chat" })).toBeInTheDocument();
  });

  it("omits the app clause from the accessible name when the app name is missing or blank", () => {
    const { unmount } = render(<OneTimeCodeButton variant="notification" hookId="notif-1" target={notificationTarget} code="839201" onCopy={vi.fn()} />);
    expect(screen.getByRole("button", { name: "Copy code 839201" })).toBeInTheDocument();
    unmount();

    render(<OneTimeCodeButton variant="notification" hookId="notif-1" target={notificationTarget} code="839201" contextName="   " onCopy={vi.fn()} />);
    expect(screen.getByRole("button", { name: "Copy code 839201" })).toBeInTheDocument();
  });

  it("calls onCopy exactly once per click with the target and code", async () => {
    const onCopy = vi.fn().mockResolvedValue(undefined);
    render(messageButton(onCopy));

    fireEvent.click(screen.getByRole("button", { name: /^Copy code 482913$/ }));

    await waitFor(() => expect(onCopy).toHaveBeenCalledTimes(1));
    expect(onCopy).toHaveBeenCalledWith(messageTarget, "482913");
  });

  it("ignores a second click while the copy is pending", async () => {
    let finish: () => void = () => undefined;
    const onCopy = vi.fn(() => new Promise<void>(resolve => { finish = resolve; }));
    render(messageButton(onCopy));
    const button = screen.getByRole("button", { name: /^Copy code 482913$/ });

    fireEvent.click(button);
    fireEvent.click(button);
    expect(onCopy).toHaveBeenCalledTimes(1);
    expect(button).toHaveAttribute("aria-disabled", "true");
    expect(button).toHaveAttribute("aria-busy", "true");
    expect(button).not.toBeDisabled();
    finish();
    await waitFor(() => expect(button).not.toHaveAttribute("aria-disabled"));
  });

  it("shows Copied and a status message after success, then reverts after 2000 ms", async () => {
    vi.useFakeTimers();
    const onCopy = vi.fn().mockResolvedValue(undefined);
    const { container } = render(messageButton(onCopy));

    fireEvent.click(screen.getByRole("button", { name: /^Copy code 482913$/ }));
    await act(async () => { await Promise.resolve(); });

    const button = screen.getByRole("button", { name: /^Copied code 482913$/ });
    expect(button).toHaveTextContent("Copied");
    expect(container.querySelector("[data-one-time-code-status]")).toHaveTextContent("Code copied");
    expect(container.querySelector("[data-one-time-code-action]")).toHaveAttribute("data-state", "copied");

    act(() => vi.advanceTimersByTime(2_000));
    expect(screen.getByRole("button", { name: /^Copy code 482913$/ })).toHaveTextContent("Copy code");
    expect(container.querySelector("[data-one-time-code-status]")).toHaveTextContent("");
    expect(container.querySelector("[data-one-time-code-action]")).toHaveAttribute("data-state", "idle");
  });

  it("re-copies when clicked during Copied without flickering back to Copy code", async () => {
    vi.useFakeTimers();
    const onCopy = vi.fn().mockResolvedValue(undefined);
    const { container } = render(messageButton(onCopy));

    fireEvent.click(screen.getByRole("button", { name: /^Copy code 482913$/ }));
    await act(async () => { await Promise.resolve(); });
    const copiedButton = screen.getByRole("button", { name: /^Copied code 482913$/ });
    fireEvent.click(copiedButton);
    // Checked before the second copy resolves: the label must not revert in between.
    expect(screen.getByRole("button", { name: /^Copied code 482913$/ })).toHaveTextContent("Copied");
    expect(screen.queryByText("Copy code")).not.toBeInTheDocument();
    await act(async () => { await Promise.resolve(); });

    expect(onCopy).toHaveBeenCalledTimes(2);
    expect(screen.getByRole("button", { name: /^Copied code 482913$/ })).toBeInTheDocument();
    expect(container.querySelector("[data-one-time-code-action]")).toHaveAttribute("data-state", "copied");
  });

  it("restarts the 2000 ms Copied timer on a re-copy", async () => {
    vi.useFakeTimers();
    const onCopy = vi.fn().mockResolvedValue(undefined);
    render(messageButton(onCopy));

    fireEvent.click(screen.getByRole("button", { name: /^Copy code 482913$/ }));
    await act(async () => { await Promise.resolve(); });
    act(() => vi.advanceTimersByTime(1_500));
    fireEvent.click(screen.getByRole("button", { name: /^Copied code 482913$/ }));
    await act(async () => { await Promise.resolve(); });
    act(() => vi.advanceTimersByTime(1_000));

    expect(screen.getByRole("button", { name: /^Copied code 482913$/ })).toBeInTheDocument();
    act(() => vi.advanceTimersByTime(1_000));
    expect(screen.getByRole("button", { name: /^Copy code 482913$/ })).toBeInTheDocument();
  });

  it("shows the generic error with role alert, keeps the button enabled, and never shows the code", async () => {
    const onCopy = vi.fn().mockRejectedValue(Object.assign(new Error("clipboard denied 482913"), { code: "clipboard-unavailable" }));
    const { container } = render(messageButton(onCopy));

    fireEvent.click(screen.getByRole("button", { name: /^Copy code 482913$/ }));

    const alert = await screen.findByRole("alert");
    expect(alert).toHaveTextContent("Couldn't copy the code. Try again.");
    expect(alert.textContent).not.toContain("482913");
    expect(alert.textContent).not.toContain("clipboard denied");
    expect(screen.queryByText("Copied")).not.toBeInTheDocument();
    expect(screen.getByRole("button", { name: /^Copy code 482913$/ })).toBeEnabled();
    expect(container.querySelector("[data-one-time-code-action]")).toHaveAttribute("data-state", "failed");
  });

  it("shows the no-longer-available text for a no-code error", async () => {
    const onCopy = vi.fn().mockRejectedValue(Object.assign(new Error("gone"), { code: "no-code" }));
    render(messageButton(onCopy));

    fireEvent.click(screen.getByRole("button", { name: /^Copy code 482913$/ }));

    expect(await screen.findByRole("alert")).toHaveTextContent("This code is no longer available.");
  });

  it("clears the error on the next click", async () => {
    const onCopy = vi.fn().mockRejectedValueOnce(new Error("denied")).mockImplementationOnce(() => new Promise<void>(() => undefined));
    render(messageButton(onCopy));
    fireEvent.click(screen.getByRole("button", { name: /^Copy code 482913$/ }));
    await screen.findByRole("alert");

    fireEvent.click(screen.getByRole("button", { name: /^Copy code 482913$/ }));

    await waitFor(() => expect(screen.queryByRole("alert")).not.toBeInTheDocument());
  });

  it("keeps focus on the button after success", async () => {
    const onCopy = vi.fn().mockResolvedValue(undefined);
    render(<><input aria-label="Other" /><OneTimeCodeButton variant="message" hookId="message-1" target={messageTarget} code="482913" onCopy={onCopy} /></>);
    const button = screen.getByRole("button", { name: /^Copy code 482913$/ });
    button.focus();

    fireEvent.click(button);
    await screen.findByRole("button", { name: /^Copied code 482913$/ });

    expect(button).toHaveFocus();
  });

  it("exposes stable hooks for the action wrapper, button, status and error", async () => {
    const onCopy = vi.fn().mockRejectedValue(new Error("denied"));
    const { container } = render(messageButton(onCopy));
    const action = container.querySelector("[data-one-time-code-action='message-1']");

    expect(action).toHaveAttribute("data-variant", "message");
    expect(action).toHaveAttribute("data-state", "idle");
    expect(container.querySelector("[data-one-time-code-button='message-1']")).toBeInTheDocument();
    expect(container.querySelector("[data-one-time-code-status]")).toBeEmptyDOMElement();

    fireEvent.click(screen.getByRole("button", { name: /^Copy code 482913$/ }));
    await screen.findByRole("alert");
    expect(container.querySelector("[data-one-time-code-error]")).toHaveAttribute("role", "alert");
  });
});
