import "@testing-library/jest-dom/vitest";
import { act, cleanup, fireEvent, render, screen } from "@testing-library/react";
import { afterEach, describe, expect, it, vi } from "vitest";
import { NotificationPhoneBlockers, type PhoneBlock } from "./NotificationPhoneBlockers";

const NOW = 1_700_000_000_000;

const block = (phoneKey: string, phoneName: string, overrides: Partial<PhoneBlock> = {}): PhoneBlock => ({
  phoneKey, phoneName, reason: "notificationAccessOff", observedAtMs: NOW - 5 * 60_000, waitingCount: 2, ...overrides,
});

afterEach(() => cleanup());

describe("NotificationPhoneBlockers", () => {
  it("refreshes report ages every minute while blocks show and stops on unmount", () => {
    vi.useFakeTimers();
    try {
      vi.setSystemTime(NOW);
      const { container, unmount } = render(<NotificationPhoneBlockers blocks={[block("p1", "Pixel")]} expandedPhones={new Set()} onTogglePhone={vi.fn()} />);
      expect(container.querySelector("#phone-blocker-p1 time")).toHaveTextContent("Reported 5m ago.");
      act(() => { vi.advanceTimersByTime(3 * 60_000); });
      expect(container.querySelector("#phone-blocker-p1 time")).toHaveTextContent("Reported 8m ago.");
      unmount();
      expect(vi.getTimerCount()).toBe(0);
    } finally {
      vi.useRealTimers();
    }
  });

  it("uses the current time when a phone first becomes blocked after mounting", () => {
    vi.useFakeTimers();
    try {
      vi.setSystemTime(NOW - 30 * 60_000);
      const { container, rerender } = render(<NotificationPhoneBlockers blocks={[]} expandedPhones={new Set()} onTogglePhone={vi.fn()} />);
      vi.setSystemTime(NOW);
      rerender(<NotificationPhoneBlockers blocks={[block("p1", "Pixel")]} expandedPhones={new Set()} onTogglePhone={vi.fn()} />);
      expect(container.querySelector("#phone-blocker-p1 time")).toHaveTextContent("Reported 5m ago.");
    } finally {
      vi.useRealTimers();
    }
  });

  it("renders nothing without blocked phones", () => {
    const { container } = render(<NotificationPhoneBlockers blocks={[]} expandedPhones={new Set()} onTogglePhone={vi.fn()} nowMs={NOW} />);
    expect(container.querySelector("#notifications-phone-blockers")).toBeNull();
  });

  it("shows one banner per phone with its reason, age and reveal toggle", () => {
    const onTogglePhone = vi.fn();
    const { container } = render(<NotificationPhoneBlockers blocks={[block("p1", "Pixel")]} expandedPhones={new Set()} onTogglePhone={onTogglePhone} nowMs={NOW} />);
    const banner = container.querySelector("#phone-blocker-p1")!;
    expect(banner).toHaveAttribute("data-phone-blocker");
    expect(banner).toHaveAttribute("data-blocker-reason", "notificationAccessOff");
    expect(banner).toHaveAttribute("data-system-ribbon-severity", "warning");
    expect(banner.querySelector("strong")).toHaveTextContent("Notification access is off on Pixel");
    expect(banner.querySelector("time")).toHaveTextContent("Reported 5m ago.");
    expect(container.querySelector("#phone-blocker-p1-help")).toBeInTheDocument();
    const toggle = container.querySelector("#phone-blocker-p1-toggle")!;
    expect(toggle).toHaveTextContent("Show 2 dismissed");
    expect(toggle).toHaveAttribute("aria-expanded", "false");
    expect(toggle).toHaveAttribute("aria-controls", "notifications-hidden-p1");
    fireEvent.click(toggle);
    expect(onTogglePhone).toHaveBeenCalledWith("p1");
  });

  it("hides the reveal toggle when nothing is waiting and shows Hide when expanded", () => {
    const { container, rerender } = render(<NotificationPhoneBlockers blocks={[block("p1", "Pixel", { waitingCount: 0 })]} expandedPhones={new Set()} onTogglePhone={vi.fn()} nowMs={NOW} />);
    expect(container.querySelector("#phone-blocker-p1-toggle")).toBeNull();
    rerender(<NotificationPhoneBlockers blocks={[block("p1", "Pixel")]} expandedPhones={new Set(["p1"])} onTogglePhone={vi.fn()} nowMs={NOW} />);
    expect(container.querySelector("#phone-blocker-p1-toggle")).toHaveTextContent("Hide 2 dismissed");
    expect(container.querySelector("#phone-blocker-p1-toggle")).toHaveAttribute("aria-expanded", "true");
  });

  it("uses the info severity for a Wi-Fi-only block", () => {
    const { container } = render(<NotificationPhoneBlockers blocks={[block("p1", "Pixel", { reason: "waitingForWifi" })]} expandedPhones={new Set()} onTogglePhone={vi.fn()} nowMs={NOW} />);
    expect(container.querySelector("#phone-blocker-p1")).toHaveAttribute("data-system-ribbon-severity", "info");
  });

  it("groups three or more phones behind an inline list that toggles open", () => {
    const onTogglePhone = vi.fn();
    const blocks = [block("p1", "Pixel"), block("p2", "Galaxy", { reason: "mirroringOff" }), block("p3", "Moto", { waitingCount: 1 })];
    const { container } = render(<NotificationPhoneBlockers blocks={blocks} expandedPhones={new Set()} onTogglePhone={onTogglePhone} nowMs={NOW} />);
    const group = container.querySelector("#phone-blockers-group")!;
    expect(group).toHaveAttribute("data-phone-blocker-group", "3");
    expect(group).toHaveAttribute("data-system-ribbon-severity", "warning");
    expect(group.querySelector("strong")).toHaveTextContent("3 phones can't clear notifications");
    expect(group.textContent).toContain("Pixel, Galaxy and Moto have 5 dismissed notifications waiting.");
    expect(container.querySelector("#phone-blockers-list")).not.toBeVisible();
    const toggle = container.querySelector("#phone-blockers-group-toggle")!;
    expect(toggle).toHaveAttribute("aria-controls", "phone-blockers-list");
    fireEvent.click(toggle);
    expect(toggle).toHaveAttribute("aria-expanded", "true");
    expect(toggle).toHaveTextContent("Hide phones");
    const list = screen.getByRole("list", { name: "Blocked phones" });
    expect(list.querySelectorAll("li[data-phone-blocker]")).toHaveLength(3);
    expect(container.querySelector("#phone-blocker-p2")).toHaveAttribute("data-blocker-reason", "mirroringOff");
    fireEvent.click(container.querySelector("#phone-blocker-p2-toggle")!);
    expect(onTogglePhone).toHaveBeenCalledWith("p2");
    expect(container.querySelector("#phone-blocker-p3-toggle")).toHaveTextContent("Show 1 dismissed");
  });

  it("titles an all-Wi-Fi group as waiting for Wi-Fi", () => {
    const blocks = ["p1", "p2", "p3"].map(key => block(key, key, { reason: "waitingForWifi" }));
    const { container } = render(<NotificationPhoneBlockers blocks={blocks} expandedPhones={new Set()} onTogglePhone={vi.fn()} nowMs={NOW} />);
    expect(container.querySelector("#phone-blockers-group")).toHaveAttribute("data-system-ribbon-severity", "info");
    expect(container.querySelector("#phone-blockers-group strong")).toHaveTextContent("3 phones are waiting for Wi-Fi");
  });
});
