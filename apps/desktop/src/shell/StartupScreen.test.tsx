import { act, cleanup, fireEvent, render, screen } from "@testing-library/react";
import "@testing-library/jest-dom/vitest";
import { afterEach, describe, expect, it, vi } from "vitest";
import { formatDuration, StartupScreen } from "./StartupScreen";

afterEach(() => { cleanup(); vi.useRealTimers(); });

describe("StartupScreen", () => {
  it("shows slow details after ten seconds without announcing ticks", () => {
    vi.useFakeTimers();
    render(<StartupScreen variant="main" progress={{ phase: "loading", elapsedMs: 9000, steps: [{ phase: "opening", durationMs: 400 }] }} />);
    expect(document.getElementById("startup-title")).toHaveTextContent("Loading conversations…");
    expect(document.getElementById("startup-steps")).toBeNull();
    act(() => vi.advanceTimersByTime(1000));
    expect(document.getElementById("startup-steps")).toBeVisible();
    const announcement = document.getElementById("startup-announcer")?.textContent;
    act(() => vi.advanceTimersByTime(1000));
    expect(announcement).toBe("Loading conversations… This is taking longer than usual. Peppy is still working.");
    expect(document.getElementById("startup-announcer")?.textContent).toBe(announcement);
  });

  it("shows update counts, omits steps in composer, and retries errors", () => {
    const retry = vi.fn();
    const { container, rerender } = render(<StartupScreen variant="composer" progress={{ phase: "updating", elapsedMs: 12000, steps: [], update: { completed: 12400, total: 48000 } }} />);
    expect(screen.getByText("12,400 of 48,000")).toBeVisible();
    expect(screen.getByText("This happens once after an update.")).toBeVisible();
    expect(container.querySelector("#startup-steps")).toBeNull();
    rerender(<StartupScreen variant="composer" error={{ title: "Error", message: "Failed", onRetry: retry }} />);
    fireEvent.click(screen.getByRole("button", { name: "Try again" }));
    expect(retry).toHaveBeenCalledOnce();
    expect(container.querySelector("#startup-screen")).toHaveFocus();
  });

  it.each(["ready", "missing"] as const)("uses mount time for %s and never lists ready steps", phase => {
    vi.useFakeTimers();
    const progress = phase === "ready" ? { phase: "ready" as const, elapsedMs: 600000, steps: [{ phase: "opening" as const, durationMs: 400 }] } : undefined;
    const { container, unmount } = render(<StartupScreen variant="main" progress={progress} />);
    expect(container.querySelector("#startup-screen")).toHaveAttribute("data-startup-slow", "false");
    act(() => vi.advanceTimersByTime(10000));
    expect(container.querySelector("#startup-screen")).toHaveAttribute("data-startup-slow", "true");
    expect(container.querySelector("#startup-steps")).toBeNull();
    unmount();
    expect(vi.getTimerCount()).toBe(0);
  });

  it("formats duration boundaries", () => {
    expect([99, 100, 9999, 10000, 60000, 65000].map(formatDuration))
      .toEqual(["<0.1 s", "0.1 s", "10.0 s", "10 s", "1 min 0 s", "1 min 5 s"]);
  });

  it("latches slow and does not duplicate an already completed current phase", () => {
    vi.useFakeTimers();
    const { container, rerender } = render(<StartupScreen variant="main" progress={{ phase: "opening", elapsedMs: 10000, steps: [{ phase: "opening", durationMs: 400 }] }} />);
    expect(container.querySelectorAll("#startup-step-opening")).toHaveLength(1);
    rerender(<StartupScreen variant="main" progress={{ phase: "loading", elapsedMs: 100, steps: [] }} />);
    expect(container.querySelector("#startup-screen")).toHaveAttribute("data-startup-slow", "true");
  });

  it("stops its timer while displaying an error", () => {
    vi.useFakeTimers();
    const { rerender } = render(<StartupScreen variant="main" />);
    expect(vi.getTimerCount()).toBe(1);
    rerender(<StartupScreen variant="main" error={{ title: "Error", message: "Failed", onRetry: vi.fn() }} />);
    expect(vi.getTimerCount()).toBe(0);
  });
});
