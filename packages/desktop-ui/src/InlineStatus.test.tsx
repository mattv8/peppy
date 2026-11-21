import "@testing-library/jest-dom/vitest";
import { act, cleanup, fireEvent, render, screen } from "@testing-library/react";
import { useState } from "react";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { InlineStatus } from "./InlineStatus";

afterEach(cleanup);

describe("InlineStatus", () => {
  const items = [{ key: "blocked", tone: "warning" as const, message: "Check your phone.", detail: "Your draft is kept." }];

  it("opens after hover and closes after leaving", () => {
    vi.useFakeTimers();
    render(<InlineStatus items={items} />);
    const trigger = screen.getByRole("button", { name: "Check your phone." });
    fireEvent.pointerEnter(trigger, { pointerType: "mouse" });
    act(() => vi.advanceTimersByTime(349));
    expect(screen.queryByRole("dialog")).toBeNull();
    act(() => vi.advanceTimersByTime(1));
    expect(screen.getByRole("dialog")).toBeInTheDocument();
    fireEvent.pointerLeave(trigger, { pointerType: "mouse" });
    act(() => vi.advanceTimersByTime(150));
    expect(screen.queryByRole("dialog")).toBeNull();
    vi.useRealTimers();
  });

  it("pins with keyboard and returns focus on Escape", () => {
    render(<><textarea id="composer-textarea" /><InlineStatus items={[{ ...items[0], actions: [{ id: "retry", label: "Try again", onAction() {} }] }]} /></>);
    const trigger = screen.getByRole("button", { name: "Check your phone." });
    trigger.focus();
    fireEvent.keyDown(trigger, { key: "Enter" });
    fireEvent.click(trigger, { detail: 0 });
    expect(screen.getByRole("dialog")).toBeInTheDocument();
    fireEvent.keyDown(screen.getByRole("dialog"), { key: "Escape" });
    expect(trigger).toHaveFocus();
  });

  it("uses supplied labels", () => {
    render(<InlineStatus labels={{ more: (count) => `${count} additional`, dialog: "Status details" }} items={[items[0], { key: "extra", tone: "info", message: "More." }]} />);
    const trigger = screen.getByRole("button", { name: "Check your phone." });
    expect(document.getElementById("inline-status-more")).toHaveTextContent("1 additional");
    fireEvent.click(trigger);
    expect(screen.getByRole("dialog", { name: "Status details" })).toBeInTheDocument();
  });

  it("shows the count, actions, and secondary ids in the popover", () => {
    const action = vi.fn();
    render(<InlineStatus items={[
      { ...items[0], actions: [{ id: "retry", label: "Try again", onAction: action }] },
      { key: "extra", tone: "error", message: "Another problem.", id: "unavailable-hint" },
    ]} />);
    const trigger = screen.getByRole("button", { name: /Check your phone/ });
    expect(trigger.querySelector("[data-inline-status-more]")).toHaveAttribute("data-count", "1");
    expect(document.getElementById("unavailable-hint")).toHaveTextContent("Another problem.");
    fireEvent.click(trigger);
    expect(screen.getByRole("dialog")).toHaveTextContent("Another problem.");
    fireEvent.click(document.getElementById("retry")!);
    expect(action).toHaveBeenCalledOnce();
  });

  it("focuses Cancel first and confirms only after confirmation", () => {
    const action = vi.fn();
    render(<InlineStatus items={[{ ...items[0], actions: [{ id: "create", label: "Create again", onAction: action, confirm: { title: "Create?", body: "Duplicate.", confirmLabel: "Create again" } }] }]} />);
    fireEvent.click(screen.getByRole("button", { name: "Check your phone." }));
    fireEvent.click(document.getElementById("create")!);
    expect(screen.getByRole("alertdialog")).toBeInTheDocument();
    expect(screen.getByRole("button", { name: "Cancel" })).toHaveFocus();
    fireEvent.click(screen.getByRole("alertdialog").querySelector("button[data-inline-status-tone]")!);
    expect(action).toHaveBeenCalledOnce();
  });

  it("closes a confirmation when its action disappears", () => {
    function StatusWithRemovedAction() {
      const [visible, setVisible] = useState(true);
      return <><button type="button" onClick={() => setVisible(false)}>Remove action</button><InlineStatus items={[{ ...items[0], actions: visible ? [{ id: "create", label: "Create again", onAction() {}, confirm: { title: "Create?", body: "Duplicate.", confirmLabel: "Create again" } }] : [] }]} /></>;
    }
    render(<StatusWithRemovedAction />);
    fireEvent.click(document.getElementById("create")!);
    expect(screen.getByRole("alertdialog")).toBeInTheDocument();
    fireEvent.click(screen.getByRole("button", { name: "Remove action" }));
    expect(screen.queryByRole("alertdialog")).toBeNull();
  });
});

describe("InlineStatus truncation", () => {
  const message = "Pixel 8 isn't responding. Sending is paused until it reconnects.";
  const widths = { scroll: 332, client: 199 };
  let observed: Set<() => void>;

  beforeEach(() => {
    widths.scroll = 332;
    widths.client = 199;
    observed = new Set();
    class ResizeObserverStub {
      private readonly callback: () => void;
      constructor(callback: () => void) { this.callback = callback; }
      observe() { observed.add(this.callback); }
      disconnect() { observed.delete(this.callback); }
    }
    vi.stubGlobal("ResizeObserver", ResizeObserverStub);
    vi.spyOn(HTMLElement.prototype, "scrollWidth", "get").mockImplementation(function (this: HTMLElement) { return this.hasAttribute("data-inline-status-message") ? widths.scroll : 0; });
    vi.spyOn(HTMLElement.prototype, "clientWidth", "get").mockImplementation(function (this: HTMLElement) { return this.hasAttribute("data-inline-status-message") ? widths.client : 0; });
  });
  afterEach(() => {
    vi.unstubAllGlobals();
    vi.restoreAllMocks();
  });

  it("opens the full sentence from a truncated message-only status and closes on Escape", () => {
    render(<InlineStatus items={[{ key: "route-problem", tone: "warning", message }]} />);
    const trigger = document.querySelector<HTMLButtonElement>("[data-inline-status-trigger]");
    expect(trigger).not.toBeNull();
    fireEvent.click(trigger!, { detail: 0 });
    expect(screen.getByRole("dialog")).toHaveTextContent(message);
    fireEvent.keyDown(screen.getByRole("dialog"), { key: "Escape" });
    expect(screen.queryByRole("dialog")).toBeNull();
  });

  it("shows no trigger when the message fits and removes it once the message stops being cut off", () => {
    widths.client = 332;
    render(<InlineStatus items={[{ key: "route-problem", tone: "warning", message }]} />);
    expect(document.querySelector("[data-inline-status-trigger]")).toBeNull();
    widths.client = 199;
    act(() => [...observed].forEach((callback) => callback()));
    expect(document.querySelector("[data-inline-status-trigger]")).not.toBeNull();
    widths.client = 332;
    act(() => [...observed].forEach((callback) => callback()));
    expect(document.querySelector("[data-inline-status-trigger]")).toBeNull();
  });

  it("moves focus to the composer when a focused or open status stops being cut off", () => {
    render(<><textarea id="composer-textarea" /><InlineStatus items={[{ key: "route-problem", tone: "warning", message }]} /></>);
    const composer = document.getElementById("composer-textarea");
    document.querySelector<HTMLButtonElement>("[data-inline-status-trigger]")!.focus();
    widths.client = 332;
    act(() => [...observed].forEach((callback) => callback()));
    expect(composer).toHaveFocus();

    composer!.blur();
    widths.client = 199;
    act(() => [...observed].forEach((callback) => callback()));
    fireEvent.click(document.querySelector("[data-inline-status-trigger]")!, { detail: 0 });
    screen.getByRole("dialog").focus();
    widths.client = 332;
    act(() => [...observed].forEach((callback) => callback()));
    expect(screen.queryByRole("dialog")).toBeNull();
    expect(composer).toHaveFocus();
  });

  it("leaves focus alone when an unfocused status stops being cut off", () => {
    render(<><textarea id="composer-textarea" /><button id="elsewhere" /><InlineStatus items={[{ key: "route-problem", tone: "warning", message }]} /></>);
    document.getElementById("elsewhere")!.focus();
    widths.client = 332;
    act(() => [...observed].forEach((callback) => callback()));
    expect(document.getElementById("elsewhere")).toHaveFocus();
  });
});

describe("InlineStatus CSS contracts", () => {
  it("exempts inline status controls from toolbar button sizing without bare tone attributes", async () => {
    // @ts-expect-error This package intentionally omits Node types; Vitest runs in Node.
    const { readFileSync } = await import("node:fs");
    const css = readFileSync("src/styles.css", "utf8");
    expect(css).toMatch(/#composer-toolbar #composer-inline-status :is\(\[data-inline-status-trigger\], \[data-inline-status-action\], \.notification-reply-recovery-action\)/);
    render(<InlineStatus items={[{ key: "status", tone: "warning", message: "Check your phone." }]} />);
    expect(document.querySelector("[data-inline-status-item]")).not.toHaveAttribute("data-tone");
  });

  it("lets the composer status shrink to its floor and restores action widths from 560px", async () => {
    // @ts-expect-error This package intentionally omits Node types; Vitest runs in Node.
    const { readFileSync } = await import("node:fs");
    const css = readFileSync("src/styles.css", "utf8");
    expect(css).toMatch(/#composer-inline-status:has\(\[data-inline-status-item\]\)\s*\{\s*min-width:\s*calc\(\s*12px\s*\+\s*4px\s*\+\s*12ch\s*\);?\s*\}/);
    expect(css).toMatch(/#composer-toolbar #composer-inline-status :is\(\[data-inline-status\],\s*\[data-inline-status-item\]\)\s*\{\s*min-width:\s*0;?\s*\}/);
    expect(css).toMatch(/@container\s*\(\s*width\s*>=\s*560px\s*\)\s*\{[^}]*#composer-toolbar #composer-inline-status:has\(\[data-inline-status-action\]\)\s*\{\s*min-width:\s*min-content;?\s*\}/);
  });
});
