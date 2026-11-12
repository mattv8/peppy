import "@testing-library/jest-dom/vitest";
import { act, cleanup, fireEvent, render, screen } from "@testing-library/react";
import { useState } from "react";
import { afterEach, describe, expect, it, vi } from "vitest";
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

describe("InlineStatus CSS contracts", () => {
  it("exempts inline status controls from toolbar button sizing without bare tone attributes", async () => {
    // @ts-expect-error This package intentionally omits Node types; Vitest runs in Node.
    const { readFileSync } = await import("node:fs");
    const css = readFileSync("src/styles.css", "utf8");
    expect(css).toMatch(/#composer-toolbar #composer-inline-status :is\(\[data-inline-status-trigger\], \[data-inline-status-action\], \.notification-reply-recovery-action\)/);
    render(<InlineStatus items={[{ key: "status", tone: "warning", message: "Check your phone." }]} />);
    expect(document.querySelector("[data-inline-status-item]")).not.toHaveAttribute("data-tone");
  });
});
