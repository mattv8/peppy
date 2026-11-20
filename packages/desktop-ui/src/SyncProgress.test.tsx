import "@testing-library/jest-dom/vitest";
import { StrictMode, useLayoutEffect, useState } from "react";
import { act, cleanup, render, screen } from "@testing-library/react";
import { afterEach, describe, expect, it, vi } from "vitest";
import type { SyncProgress as SyncProgressSnapshot } from "./bridge-contracts";
import { HIDE_LINGER_MS, SHOW_DELAY_MS, SyncProgress } from "./SyncProgress";

const HUGE_RECEIVED = "123456789012345678901234567890";
const HUGE_PROCESSED = "123456789012345678901234567000";
const HUGE_TARGET = "123456789012345678901234568000";

const progress = (overrides: Partial<SyncProgressSnapshot> = {}): SyncProgressSnapshot => ({
  phase: "receiving",
  receivedThrough: HUGE_RECEIVED,
  processedThrough: HUGE_PROCESSED,
  targetHighWater: HUGE_TARGET,
  ...overrides,
});

const banner = () => document.querySelector<HTMLElement>("[data-sync-progress]");
const announcement = () => document.querySelector<HTMLElement>("[data-sync-announcement]");
const region = () => screen.queryByRole("region", { name: "History sync" });

function SyncProgressThatBecomesCurrent() {
  const [syncProgress, setSyncProgress] = useState(progress());
  useLayoutEffect(() => {
    setSyncProgress(progress({ phase: "current" }));
  }, []);
  return <SyncProgress progress={syncProgress} />;
}

afterEach(() => {
  cleanup();
  vi.useRealTimers();
});

describe("SyncProgress", () => {
  it("renders nothing visible when progress is absent or current", () => {
    const { rerender } = render(<SyncProgress />);
    expect(banner()).toBeNull();
    expect(region()).toBeNull();
    rerender(<SyncProgress progress={progress({ phase: "current" })} />);
    expect(banner()).toBeNull();
    expect(region()).toBeNull();
  });

  it("shows a one-line truthful receiving label", () => {
    render(<SyncProgress progress={progress()} />);
    const section = region();
    expect(section).toHaveAttribute("data-sync-phase", "receiving");
    const title = section!.querySelector("[data-sync-title]");
    expect(title).toHaveTextContent("Loading your history…");
    expect(title?.textContent).toBe("Loading your history…");
  });

  it("shows receiving counts as a determinate progress bar", () => {
    render(<SyncProgress progress={progress({ snapshotReceived: "42000", snapshotExpected: "108882" })} />);
    expect(screen.getByRole("progressbar", { name: "Loading your history" })).toHaveAttribute("value", "42000");
    expect(screen.getByRole("progressbar", { name: "Loading your history" })).toHaveAttribute("max", "108882");
    expect(document.querySelector("[data-sync-count]")).toHaveTextContent("42,000 of 108,882 items");
  });

  it("shows no organizing bar without a total", () => {
    const { rerender } = render(<SyncProgress progress={progress()} />);
    expect(screen.queryByRole("progressbar")).toBeNull();
    expect(document.querySelector("[data-sync-count]")).toBeNull();
    rerender(<SyncProgress progress={progress({ phase: "applying" })} />);
    expect(screen.queryByRole("progressbar")).toBeNull();
    expect(document.querySelector("[data-sync-count]")).toBeNull();
  });

  it("shows the completed portion of a known organizing total", () => {
    render(<SyncProgress progress={progress({ phase: "applying", snapshotRemaining: "25", snapshotTotal: "100" })} />);
    const bar = screen.getByRole("progressbar", { name: "Organizing downloaded history" });
    expect(bar).toHaveAttribute("value", "75");
    expect(bar).toHaveAttribute("max", "100");
    expect(document.querySelector("[data-sync-count]")).toHaveTextContent("75 of 100 items");
  });

  it("holds the last organizing ratio when the total temporarily disappears, but resets on leaving applying", () => {
    const { rerender } = render(<SyncProgress progress={progress({ phase: "applying", snapshotRemaining: "25", snapshotTotal: "100" })} />);
    rerender(<SyncProgress progress={progress({ phase: "applying", snapshotRemaining: "20" })} />);
    expect(screen.getByRole("progressbar")).toHaveAttribute("value", "75");
    rerender(<SyncProgress progress={progress({ phase: "receiving" })} />);
    rerender(<SyncProgress progress={progress({ phase: "applying", snapshotRemaining: "20" })} />);
    expect(screen.queryByRole("progressbar")).toBeNull();
  });

  it.each([["0", "0"], ["10", "11"], ["10", "-1"], ["9007199254740992", "1"], ["10", "invalid"], ["10", ""]])(
    "omits the organizing ratio for invalid total %s and remaining %s", (snapshotTotal, snapshotRemaining) => {
      render(<SyncProgress progress={progress({ phase: "applying", snapshotTotal, snapshotRemaining })} />);
      expect(screen.queryByRole("progressbar")).toBeNull();
    },
  );

  it("delays repair and displays a static repair state", () => {
    vi.useFakeTimers();
    const { rerender } = render(<SyncProgress />);
    rerender(<SyncProgress progress={progress({ phase: "repairing" })} />);
    expect(region()).toBeNull();
    act(() => vi.advanceTimersByTime(SHOW_DELAY_MS));
    expect(region()).toHaveAttribute("data-sync-phase", "repairing");
    expect(region()?.querySelector(".lucide-clock")).not.toBeNull();
    expect(region()?.querySelector(".busy-indicator-icon")).toBeNull();
    expect(region()).toHaveTextContent("History is incomplete");
    expect(region()).toHaveAccessibleDescription("A repair is waiting to run. Some history may not show until it finishes.");
    expect(screen.queryByRole("progressbar")).toBeNull();
    expect(announcement()).toHaveTextContent("History is incomplete. A repair is waiting to run.");
  });

  it("suppresses repair that clears before the delay, including on initial mount", () => {
    vi.useFakeTimers();
    const { rerender } = render(<SyncProgress progress={progress({ phase: "repairing" })} />);
    expect(region()).toBeNull();
    act(() => vi.advanceTimersByTime(SHOW_DELAY_MS - 1));
    rerender(<SyncProgress progress={progress({ phase: "current" })} />);
    act(() => vi.advanceTimersByTime(SHOW_DELAY_MS));
    expect(region()).toBeNull();
    expect(announcement()).toHaveTextContent("");
  });

  it("delays blocked, persists while blocked, and clears without a completion confirmation", () => {
    vi.useFakeTimers();
    const { rerender } = render(<SyncProgress />);
    rerender(<SyncProgress progress={progress({ phase: "blocked" })} />);
    expect(region()).toBeNull();
    act(() => vi.advanceTimersByTime(SHOW_DELAY_MS));
    expect(region()).toHaveAttribute("data-sync-phase", "blocked");
    expect(region()?.querySelector(".lucide-triangle-alert")).not.toBeNull();
    expect(region()?.querySelector(".busy-indicator-icon")).toBeNull();
    expect(region()).toHaveTextContent("Couldn't finish loading history");
    expect(region()).toHaveAccessibleDescription("Some history may be missing.");
    expect(announcement()).toHaveTextContent("Peppy couldn't finish loading your history. Some history may be missing.");
    act(() => vi.advanceTimersByTime(HIDE_LINGER_MS * 2));
    expect(region()).toHaveAttribute("data-sync-phase", "blocked");
    rerender(<SyncProgress progress={progress({ phase: "current" })} />);
    expect(region()).toBeNull();
    act(() => vi.advanceTimersByTime(HIDE_LINGER_MS));
    expect(region()).toBeNull();
    expect(announcement()).not.toHaveTextContent("Your history is up to date.");
  });

  it("shows the latest active phase and announces each swap, then confirms repair completion", () => {
    vi.useFakeTimers();
    const { rerender } = render(<SyncProgress progress={progress()} />);
    for (const phase of ["blocked", "repairing", "applying", "receiving", "repairing"] as const) {
      rerender(<SyncProgress progress={progress({ phase })} />);
      expect(region()).toHaveAttribute("data-sync-phase", phase);
    }
    expect(announcement()).toHaveTextContent("History is incomplete. A repair is waiting to run.");
    rerender(<SyncProgress progress={progress({ phase: "current" })} />);
    expect(region()).toHaveAttribute("data-sync-phase", "current");
    expect(region()).not.toHaveAttribute("aria-describedby");
    act(() => vi.advanceTimersByTime(HIDE_LINGER_MS));
    expect(announcement()).toHaveTextContent("Your history is up to date.");
  });

  it("suppresses an initial blocked blip", () => {
    vi.useFakeTimers();
    const { rerender } = render(<SyncProgress progress={progress({ phase: "blocked" })} />);
    expect(region()).toBeNull();
    rerender(<SyncProgress progress={progress({ phase: "current" })} />);
    act(() => vi.advanceTimersByTime(SHOW_DELAY_MS));
    expect(region()).toBeNull();
    expect(announcement()).toHaveTextContent("");
  });

  it("does not confirm completion when blocked progress becomes absent", () => {
    vi.useFakeTimers();
    const { rerender } = render(<SyncProgress progress={progress({ phase: "blocked" })} />);
    act(() => vi.advanceTimersByTime(SHOW_DELAY_MS));
    rerender(<SyncProgress />);
    expect(region()).toBeNull();
    act(() => vi.advanceTimersByTime(HIDE_LINGER_MS));
    expect(announcement()).not.toHaveTextContent("Your history is up to date.");
  });

  it("falls back to the plain banner when the download total is zero", () => {
    render(<SyncProgress progress={progress({ snapshotReceived: "0", snapshotExpected: "0" })} />);
    expect(screen.queryByRole("progressbar")).toBeNull();
    expect(document.querySelector("[data-sync-count]")).toBeNull();
    expect(region()).toHaveAttribute("data-sync-phase", "receiving");
  });

  it("shows the organizing title when the target is unknown", () => {
    render(<SyncProgress progress={progress({ phase: "applying", targetHighWater: undefined })} />);
    expect(region()).toHaveAttribute("data-sync-phase", "applying");
    expect(region()!.querySelector("[data-sync-title]")?.textContent).toBe("Organizing downloaded history…");
  });

  it("keeps waiting-for-keys visibly noncomplete", () => {
    render(<SyncProgress progress={progress({ phase: "waitingForKeys" })} />);
    const section = region();
    expect(section).toHaveAttribute("data-sync-phase", "waitingForKeys");
    expect(section!.querySelector("[data-sync-title]")?.textContent).toBe("Waiting for vault keys to continue loading history…");
    expect(section!.textContent).not.toMatch(/complete|up to date|finished|done/i);
  });

  it("does not render a details disclosure in any active phase", () => {
    const states = [
      progress({ snapshotReceived: "42", snapshotExpected: "108" }),
      progress(),
      progress({ phase: "applying" }),
      progress({ phase: "waitingForKeys" }),
      progress({ phase: "repairing" }),
      progress({ phase: "blocked" }),
    ];
    const { rerender } = render(<SyncProgress progress={states[0]} />);
    for (const state of states) {
      rerender(<SyncProgress progress={state} />);
      expect(region()).toHaveAttribute("data-sync-phase", state.phase);
      expect(region()!.querySelector("details")).toBeNull();
    }
  });

  it("does not steal focus when progress updates", () => {
    const { rerender } = render(<><input aria-label="Search" /><SyncProgress progress={progress()} /></>);
    const input = screen.getByRole("textbox", { name: "Search" });
    input.focus();
    expect(input).toHaveFocus();
    rerender(<><input aria-label="Search" /><SyncProgress progress={progress({ phase: "applying", processedThrough: "10" })} /></>);
    expect(screen.getByRole("textbox", { name: "Search" })).toHaveFocus();
    expect(banner()?.querySelectorAll("button, [tabindex], a[href], input")).toHaveLength(0);
  });

  it("announces displayed phase changes only, not cursor-only updates", () => {
    vi.useFakeTimers();
    const { rerender } = render(<SyncProgress progress={progress()} />);
    expect(announcement()?.textContent).toBe("Loading your history.");
    rerender(<SyncProgress progress={progress({ receivedThrough: "200" })} />);
    expect(announcement()?.textContent).toBe("Loading your history.");
    rerender(<SyncProgress progress={progress({ phase: "applying", processedThrough: "150" })} />);
    expect(announcement()?.textContent).toBe("Organizing downloaded history.");
    rerender(<SyncProgress progress={progress({ phase: "applying", processedThrough: "160" })} />);
    expect(announcement()?.textContent).toBe("Organizing downloaded history.");
    rerender(<SyncProgress progress={progress({ phase: "current" })} />);
    expect(banner()).not.toBeNull();
    act(() => vi.advanceTimersByTime(HIDE_LINGER_MS));
    expect(banner()).toBeNull();
    expect(announcement()?.textContent).toBe("Your history is up to date.");
  });

  it("suppresses active blips shorter than the show delay", () => {
    vi.useFakeTimers();
    const { rerender } = render(<SyncProgress />);
    rerender(<SyncProgress progress={progress()} />);
    act(() => vi.advanceTimersByTime(SHOW_DELAY_MS - 1));
    rerender(<SyncProgress progress={progress({ phase: "current" })} />);
    act(() => vi.advanceTimersByTime(SHOW_DELAY_MS));
    expect(banner()).toBeNull();
    expect(announcement()).toHaveTextContent("");
  });

  it("shows an active phase after the show delay", () => {
    vi.useFakeTimers();
    const { rerender } = render(<SyncProgress />);
    rerender(<SyncProgress progress={progress()} />);
    act(() => vi.advanceTimersByTime(SHOW_DELAY_MS));
    expect(region()).toHaveAttribute("data-sync-phase", "receiving");
    expect(announcement()).toHaveTextContent("Loading your history.");
  });

  it("does not reset the show delay when active phases switch", () => {
    vi.useFakeTimers();
    const { rerender } = render(<SyncProgress />);
    rerender(<SyncProgress progress={progress()} />);
    act(() => vi.advanceTimersByTime(SHOW_DELAY_MS - 100));
    rerender(<SyncProgress progress={progress({ phase: "applying" })} />);
    act(() => vi.advanceTimersByTime(100));
    expect(region()).toHaveAttribute("data-sync-phase", "applying");
  });

  it("lingers after current, then hides and announces completion", () => {
    vi.useFakeTimers();
    const { rerender } = render(<SyncProgress progress={progress()} />);
    rerender(<SyncProgress progress={progress({ phase: "current", snapshotReceived: "2", snapshotExpected: "10" })} />);
    expect(region()).toHaveAttribute("data-sync-phase", "current");
    expect(region()).toHaveTextContent("History is up to date");
    expect(region()?.querySelector(".lucide-check")).not.toBeNull();
    expect(region()?.querySelector(".busy-indicator-icon")).toBeNull();
    expect(screen.queryByRole("progressbar")).toBeNull();
    expect(document.querySelector("[data-sync-count]")).toBeNull();
    act(() => vi.advanceTimersByTime(HIDE_LINGER_MS - 1));
    expect(banner()).not.toBeNull();
    act(() => vi.advanceTimersByTime(1));
    expect(banner()).toBeNull();
    expect(announcement()).toHaveTextContent("Your history is up to date.");
  });

  it("cancels a pending hide when sync becomes active again", () => {
    vi.useFakeTimers();
    const { rerender } = render(<SyncProgress progress={progress()} />);
    rerender(<SyncProgress progress={progress({ phase: "current" })} />);
    act(() => vi.advanceTimersByTime(HIDE_LINGER_MS - 1));
    rerender(<SyncProgress progress={progress({ phase: "applying" })} />);
    act(() => vi.advanceTimersByTime(HIDE_LINGER_MS));
    expect(region()).toHaveAttribute("data-sync-phase", "applying");
  });

  it("shows waiting for keys immediately", () => {
    vi.useFakeTimers();
    const { rerender } = render(<SyncProgress />);
    rerender(<SyncProgress progress={progress({ phase: "waitingForKeys" })} />);
    expect(region()).toHaveAttribute("data-sync-phase", "waitingForKeys");
  });

  it("hides absent progress after lingering without a completion announcement", () => {
    vi.useFakeTimers();
    const { rerender } = render(<SyncProgress progress={progress()} />);
    rerender(<SyncProgress />);
    expect(region()).toHaveAttribute("data-sync-phase", "receiving");
    expect(region()).not.toHaveTextContent("History is up to date");
    act(() => vi.advanceTimersByTime(HIDE_LINGER_MS));
    expect(banner()).toBeNull();
    expect(announcement()).toHaveTextContent("Loading your history.");
  });

  it("keeps the hide deadline when current and absent switch during the linger", () => {
    vi.useFakeTimers();
    const { rerender } = render(<SyncProgress progress={progress()} />);
    rerender(<SyncProgress progress={progress({ phase: "current" })} />);
    act(() => vi.advanceTimersByTime(HIDE_LINGER_MS - 100));
    rerender(<SyncProgress />);
    act(() => vi.advanceTimersByTime(100));
    expect(banner()).toBeNull();
    expect(announcement()).toHaveTextContent("Loading your history.");
  });

  it("announces completion when current is restored before the original hide deadline", () => {
    vi.useFakeTimers();
    const { rerender } = render(<SyncProgress progress={progress()} />);
    rerender(<SyncProgress />);
    act(() => vi.advanceTimersByTime(HIDE_LINGER_MS - 100));
    rerender(<SyncProgress progress={progress({ phase: "current" })} />);
    act(() => vi.advanceTimersByTime(100));
    expect(banner()).toBeNull();
    expect(announcement()).toHaveTextContent("Your history is up to date.");
  });

  it("shows an active phase on the first render", () => {
    render(<SyncProgress progress={progress({ phase: "applying" })} />);
    expect(region()).toHaveAttribute("data-sync-phase", "applying");
  });

  it("cleans up timers when unmounted", () => {
    vi.useFakeTimers();
    const clearTimeoutSpy = vi.spyOn(globalThis, "clearTimeout");
    const { rerender, unmount } = render(<SyncProgress />);
    rerender(<SyncProgress progress={progress()} />);
    unmount();
    expect(clearTimeoutSpy).toHaveBeenCalled();
  });

  it("resets timer refs across StrictMode effect cleanup", () => {
    vi.useFakeTimers();
    render(<StrictMode><SyncProgressThatBecomesCurrent /></StrictMode>);
    act(() => vi.advanceTimersByTime(HIDE_LINGER_MS));
    expect(banner()).toBeNull();
  });
});
