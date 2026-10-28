import "@testing-library/jest-dom/vitest";
import { cleanup, render, screen, within } from "@testing-library/react";
import { afterEach, describe, expect, it } from "vitest";
import type { SyncProgress as SyncProgressSnapshot } from "./bridge-contracts";
import { SyncProgress } from "./SyncProgress";

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
const details = () => document.querySelector<HTMLDetailsElement>("[data-sync-details]");

afterEach(() => {
  cleanup();
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

  it("shows a one-line truthful receiving label with cursors collapsed", () => {
    render(<SyncProgress progress={progress()} />);
    const section = region();
    expect(section).toHaveAttribute("data-sync-phase", "receiving");
    const title = section!.querySelector("[data-sync-title]");
    expect(title).toHaveTextContent("Loading your history…");
    expect(title?.textContent).toBe("Loading your history…");
    expect(details()).not.toHaveAttribute("open");
  });

  it("keeps huge cursor strings exact inside the details disclosure", () => {
    render(<SyncProgress progress={progress()} />);
    const disclosure = within(details()!);
    expect(disclosure.getByText(HUGE_RECEIVED)).toHaveAttribute("data-sync-received");
    expect(disclosure.getByText(HUGE_PROCESSED)).toHaveAttribute("data-sync-processed");
    expect(disclosure.getByText(HUGE_TARGET)).toHaveAttribute("data-sync-target");
    expect(disclosure.getByText(HUGE_RECEIVED).textContent).toBe(HUGE_RECEIVED);
  });

  it("keeps very large decimal cursors exact without Number conversion", () => {
    const received = "18446744073709551616000000000000000001";
    render(<SyncProgress progress={progress({ receivedThrough: received, processedThrough: "0", targetHighWater: "18446744073709551616000000000000000009" })} />);
    const disclosure = within(details()!);
    expect(disclosure.getByText(received)).toBeInTheDocument();
    expect(disclosure.getByText("0")).toBeInTheDocument();
    expect(disclosure.getByText("18446744073709551616000000000000000009")).toBeInTheDocument();
  });

  it("omits the target row when the target is unknown", () => {
    render(<SyncProgress progress={progress({ phase: "applying", targetHighWater: undefined })} />);
    expect(region()).toHaveAttribute("data-sync-phase", "applying");
    expect(region()!.querySelector("[data-sync-target]")).toBeNull();
    expect(region()!.querySelector("[data-sync-title]")?.textContent).toBe("Organizing downloaded history…");
  });

  it("keeps waiting-for-keys visibly noncomplete", () => {
    render(<SyncProgress progress={progress({ phase: "waitingForKeys" })} />);
    const section = region();
    expect(section).toHaveAttribute("data-sync-phase", "waitingForKeys");
    expect(section!.querySelector("[data-sync-title]")?.textContent).toBe("Waiting for vault keys to continue loading history…");
    expect(section!.textContent).not.toMatch(/complete|up to date|finished|done/i);
  });

  it("uses the availability note instead of claiming existing conversations", () => {
    render(<SyncProgress progress={progress()} />);
    const section = region()!;
    expect(section.textContent).toContain("You can use conversations as they become available.");
    expect(section.textContent).not.toMatch(/already have|stay available/i);
    expect(section.textContent).not.toMatch(/%|messages?\b/i);
  });

  it("has an accessible native disclosure with no focus stealing when progress updates", () => {
    const { rerender } = render(<><input aria-label="Search" /><SyncProgress progress={progress()} /></>);
    const summary = screen.getByText("Sync details");
    expect(summary.tagName).toBe("SUMMARY");
    expect(summary).not.toHaveAttribute("tabindex", "-1");
    const input = screen.getByRole("textbox", { name: "Search" });
    input.focus();
    expect(input).toHaveFocus();
    rerender(<><input aria-label="Search" /><SyncProgress progress={progress({ phase: "applying", processedThrough: "10" })} /></>);
    expect(screen.getByRole("textbox", { name: "Search" })).toHaveFocus();
    expect(banner()?.querySelectorAll("button, [tabindex], a[href], input")).toHaveLength(0);
  });

  it("announces phase changes only, not cursor-only updates", () => {
    const { rerender } = render(<SyncProgress progress={progress()} />);
    expect(announcement()?.textContent).toBe("");
    rerender(<SyncProgress progress={progress({ receivedThrough: "200" })} />);
    expect(announcement()?.textContent).toBe("");
    rerender(<SyncProgress progress={progress({ phase: "applying", processedThrough: "150" })} />);
    expect(announcement()?.textContent).toBe("Organizing downloaded history.");
    rerender(<SyncProgress progress={progress({ phase: "applying", processedThrough: "160" })} />);
    expect(announcement()?.textContent).toBe("Organizing downloaded history.");
    rerender(<SyncProgress progress={progress({ phase: "current" })} />);
    expect(banner()).toBeNull();
    expect(announcement()?.textContent).toBe("Your history is up to date.");
  });
});
