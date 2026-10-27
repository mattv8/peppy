import "@testing-library/jest-dom/vitest";
import { cleanup, fireEvent, render, screen, waitFor } from "@testing-library/react";
import { afterEach, describe, expect, it, vi } from "vitest";
import { MessageText } from "./MessageText";

const originalClipboard = navigator.clipboard;

function deferred<T>() {
  let resolve!: (value: T | PromiseLike<T>) => void;
  let reject!: (reason?: unknown) => void;
  const promise = new Promise<T>((resolvePromise, rejectPromise) => {
    resolve = resolvePromise;
    reject = rejectPromise;
  });
  return { promise, resolve, reject };
}

afterEach(() => {
  cleanup();
  Object.defineProperty(navigator, "clipboard", { configurable: true, value: originalClipboard });
});

describe("MessageText", () => {
  it("renders normal browser anchors without changing message text", () => {
    const value = "Visit www.example.com, then https://example.com/a.";
    const { container } = render(<MessageText text={value} />);
    const links = screen.getAllByRole("link");

    expect(container.querySelector("[data-message-text]")).toHaveTextContent(value);
    expect(links[0]).toHaveAttribute("href", "https://www.example.com/");
    expect(links[0]).toHaveAttribute("target", "_blank");
    expect(links[0]).toHaveAttribute("rel", "noopener noreferrer");
    expect(links[1]).toHaveAttribute("href", "https://example.com/a");
  });

  it("preserves a long URL's full label, accessible name, and destination bindings", () => {
    const longUrl = `https://example.com/${"a".repeat(1200)}`;
    const message = `Read ${longUrl} and keep reading.`;
    const { container } = render(<MessageText text={message} />);
    const link = screen.getByRole("link", { name: longUrl });
    const label = link.querySelector("[data-message-text-link-label]");

    expect(container.querySelector("[data-message-text]")).toHaveTextContent(message);
    expect(label).toHaveTextContent(longUrl);
    expect(link).toHaveAttribute("href", longUrl);
    expect(link).toHaveAttribute("title", longUrl);
    expect(link).toHaveAttribute("data-message-text-destination", longUrl);
  });

  it("opens the full long URL once for clicks that bubble from its label", () => {
    const longUrl = `https://example.com/${"a".repeat(1200)}`;
    const onOpenExternal = vi.fn(async () => undefined);
    render(<MessageText text={longUrl} onOpenExternal={onOpenExternal} />);
    const label = screen.getByRole("link", { name: longUrl }).querySelector("[data-message-text-link-label]");
    const click = new MouseEvent("click", { bubbles: true, cancelable: true });
    const middleClick = new MouseEvent("auxclick", { bubbles: true, cancelable: true, button: 1 });

    label!.dispatchEvent(click);
    label!.dispatchEvent(middleClick);

    expect(click.defaultPrevented).toBe(true);
    expect(middleClick.defaultPrevented).toBe(true);
    expect(onOpenExternal).toHaveBeenCalledTimes(2);
    expect(onOpenExternal).toHaveBeenNthCalledWith(1, longUrl);
    expect(onOpenExternal).toHaveBeenNthCalledWith(2, longUrl);
  });

  it("keeps browser click and middle-click default navigation intact", () => {
    render(<MessageText text="https://example.com" />);
    const link = screen.getByRole("link");
    const click = new MouseEvent("click", { bubbles: true, cancelable: true });
    const middleClick = new MouseEvent("auxclick", { bubbles: true, cancelable: true, button: 1 });

    link.dispatchEvent(click);
    link.dispatchEvent(middleClick);

    expect(click.defaultPrevented).toBe(false);
    expect(middleClick.defaultPrevented).toBe(false);
  });

  it("prevents native primary and modified clicks synchronously and opens once", () => {
    const onOpenExternal = vi.fn(async () => undefined);
    render(<MessageText text="https://example.com" onOpenExternal={onOpenExternal} />);
    const link = screen.getByRole("link");
    const click = new MouseEvent("click", { bubbles: true, cancelable: true, ctrlKey: true });

    link.dispatchEvent(click);

    expect(click.defaultPrevented).toBe(true);
    expect(onOpenExternal).toHaveBeenCalledTimes(1);
    expect(onOpenExternal).toHaveBeenCalledWith("https://example.com/");
  });

  it("prevents native keyboard activation and opens the destination", () => {
    const onOpenExternal = vi.fn(async () => undefined);
    render(<MessageText text="https://example.com" onOpenExternal={onOpenExternal} />);
    const link = screen.getByRole("link");
    const click = new MouseEvent("click", { bubbles: true, cancelable: true, detail: 0 });

    link.dispatchEvent(click);

    expect(click.defaultPrevented).toBe(true);
    expect(onOpenExternal).toHaveBeenCalledOnce();
  });

  it("prevents native middle-click navigation and opens once", () => {
    const onOpenExternal = vi.fn(async () => undefined);
    render(<MessageText text="https://example.com" onOpenExternal={onOpenExternal} />);
    const link = screen.getByRole("link");
    const middleClick = new MouseEvent("auxclick", { bubbles: true, cancelable: true, button: 1 });

    link.dispatchEvent(middleClick);

    expect(middleClick.defaultPrevented).toBe(true);
    expect(onOpenExternal).toHaveBeenCalledOnce();
  });

  it("opens a native middle-click once when a click event precedes auxclick", () => {
    const onOpenExternal = vi.fn(async () => undefined);
    render(<MessageText text="https://example.com" onOpenExternal={onOpenExternal} />);
    const link = screen.getByRole("link");
    const click = new MouseEvent("click", { bubbles: true, cancelable: true, button: 1 });
    const middleClick = new MouseEvent("auxclick", { bubbles: true, cancelable: true, button: 1 });

    link.dispatchEvent(click);
    link.dispatchEvent(middleClick);

    expect(click.defaultPrevented).toBe(true);
    expect(middleClick.defaultPrevented).toBe(true);
    expect(onOpenExternal).toHaveBeenCalledOnce();
  });

  it("keeps navigation prevented when native opening fails and offers safe retry and copy actions", async () => {
    const onOpenExternal = vi.fn()
      .mockRejectedValueOnce(new Error("private host details"))
      .mockResolvedValueOnce(undefined);
    const writeText = vi.fn(async () => undefined);
    Object.defineProperty(navigator, "clipboard", { configurable: true, value: { writeText } });
    render(<MessageText text="https://example.com" onOpenExternal={onOpenExternal} />);
    const link = screen.getByRole("link");
    const click = new MouseEvent("click", { bubbles: true, cancelable: true });

    link.dispatchEvent(click);

    expect(click.defaultPrevented).toBe(true);
    expect(await screen.findByRole("alert")).toHaveTextContent("Could not open link.");
    expect(screen.getByRole("alert")).not.toHaveTextContent("private host details");
    fireEvent.click(screen.getByRole("button", { name: "Copy link" }));
    await waitFor(() => expect(writeText).toHaveBeenCalledWith("https://example.com/"));
    fireEvent.click(screen.getByRole("button", { name: "Retry opening link" }));
    await waitFor(() => expect(onOpenExternal).toHaveBeenCalledTimes(2));
  });

  it("removes an opener failure when the message text changes", async () => {
    const pendingOpen = deferred<void>();
    const onOpenExternal = vi.fn(() => pendingOpen.promise);
    const { rerender } = render(<MessageText text="https://old.example.com" onOpenExternal={onOpenExternal} />);

    fireEvent.click(screen.getByRole("link"));
    rerender(<MessageText text="https://new.example.com" onOpenExternal={onOpenExternal} />);
    rerender(<MessageText text="https://old.example.com" onOpenExternal={onOpenExternal} />);
    pendingOpen.reject(new Error("old failure"));

    await waitFor(() => expect(onOpenExternal).toHaveBeenCalledOnce());
    expect(screen.queryByRole("alert")).not.toBeInTheDocument();
  });

  it("shows an inline destination on keyboard focus", () => {
    render(<MessageText text="https://例え.テスト/path" />);
    const link = screen.getByRole("link");

    fireEvent.focus(link);

    expect(link).toHaveAttribute("data-message-text-destination", "https://xn--r8jz45g.xn--zckzah/path");
    expect(link).toHaveAttribute("title", "https://xn--r8jz45g.xn--zckzah/path");
  });

  it("reports safe clipboard failure instead of leaving an unhandled rejection", async () => {
    const onOpenExternal = vi.fn().mockRejectedValueOnce(new Error("open failed"));
    const writeText = vi.fn().mockRejectedValueOnce(new Error("clipboard details"));
    Object.defineProperty(navigator, "clipboard", { configurable: true, value: { writeText } });
    render(<MessageText text="https://example.com" onOpenExternal={onOpenExternal} />);

    fireEvent.click(screen.getByRole("link"));
    await screen.findByRole("alert");
    fireEvent.click(screen.getByRole("button", { name: "Copy link" }));

    await waitFor(() => expect(screen.getByRole("alert")).toHaveTextContent("Could not copy link."));
    expect(screen.queryByText("clipboard details")).not.toBeInTheDocument();
  });

  it("reports a safe status when clipboard access is unavailable", async () => {
    const onOpenExternal = vi.fn().mockRejectedValueOnce(new Error("open failed"));
    Object.defineProperty(navigator, "clipboard", { configurable: true, value: undefined });
    render(<MessageText text="https://example.com" onOpenExternal={onOpenExternal} />);

    fireEvent.click(screen.getByRole("link"));
    await screen.findByRole("alert");
    fireEvent.click(screen.getByRole("button", { name: "Copy link" }));

    expect(screen.getByRole("alert")).toHaveTextContent("Could not copy link.");
  });

  it("clears copy status before retrying an external open", async () => {
    const retry = deferred<void>();
    const onOpenExternal = vi.fn().mockRejectedValueOnce(new Error("open failed")).mockReturnValueOnce(retry.promise);
    const writeText = vi.fn().mockRejectedValueOnce(new Error("clipboard failed"));
    Object.defineProperty(navigator, "clipboard", { configurable: true, value: { writeText } });
    render(<MessageText text="https://example.com" onOpenExternal={onOpenExternal} />);

    fireEvent.click(screen.getByRole("link"));
    await screen.findByRole("alert");
    fireEvent.click(screen.getByRole("button", { name: "Copy link" }));
    await waitFor(() => expect(screen.getByRole("alert")).toHaveTextContent("Could not copy link."));
    fireEvent.click(screen.getByRole("button", { name: "Retry opening link" }));

    expect(screen.getByRole("alert")).not.toHaveTextContent("Could not copy link.");
    retry.resolve();
  });
});
