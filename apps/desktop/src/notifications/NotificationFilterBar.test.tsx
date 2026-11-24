import { cleanup, fireEvent, render, screen } from "@testing-library/react";
import "@testing-library/jest-dom/vitest";
import { afterEach, describe, expect, it, vi } from "vitest";
import { NotificationFilterBar } from "./NotificationFilterBar";

afterEach(cleanup);

describe("NotificationFilterBar", () => {
  it("keeps arrangement names and invokes the enabled dismiss action", () => {
    const onDismissAll = vi.fn();
    const onArrangement = vi.fn();
    render(<NotificationFilterBar chips={[]} appFilter={null} onAppFilter={vi.fn()} arrangement="time" onArrangement={onArrangement} dismissAll={{ label: "Dismiss up to 100", disabled: false, onDismissAll }} />);
    expect(screen.getByRole("button", { name: "Time" })).toHaveAttribute("title", "Time");
    fireEvent.click(screen.getByRole("button", { name: "App" }));
    expect(onArrangement).toHaveBeenCalledWith("app");
    const dismiss = screen.getByRole("button", { name: "Dismiss up to 100" });
    expect(dismiss).toHaveAttribute("title", "Dismiss up to 100");
    fireEvent.click(dismiss);
    expect(onDismissAll).toHaveBeenCalledOnce();
  });

  it("disables the dismiss action when requested", () => {
    const onDismissAll = vi.fn();
    render(<NotificationFilterBar chips={[]} appFilter={null} onAppFilter={vi.fn()} arrangement="time" onArrangement={vi.fn()} dismissAll={{ label: "Dismiss all", disabled: true, onDismissAll }} />);
    const dismiss = screen.getByRole("button", { name: "Dismiss all" });
    expect(dismiss).toBeDisabled();
    fireEvent.click(dismiss);
    expect(onDismissAll).not.toHaveBeenCalled();
  });

  it("omits the dismiss-all button when no dismiss action is supplied", () => {
    const { container } = render(<NotificationFilterBar chips={[]} appFilter={null} onAppFilter={vi.fn()} arrangement="time" onArrangement={vi.fn()} />);
    expect(container.querySelector("#notifications-dismiss-all")).toBeNull();
  });

  it("places the waiting slot between the chips and the arrangement toggle", () => {
    const { container } = render(<NotificationFilterBar chips={[{ packageName: "a", name: "A", count: 1 }, { packageName: "b", name: "B", count: 1 }]} appFilter={null} onAppFilter={vi.fn()} arrangement="time" onArrangement={vi.fn()} waiting={<span id="waiting-slot">Waiting</span>} />);
    const slot = container.querySelector("#waiting-slot")!;
    expect(container.querySelector("#notifications-filter-bar")!.compareDocumentPosition(slot) & Node.DOCUMENT_POSITION_FOLLOWING).toBeTruthy();
    expect(slot.compareDocumentPosition(container.querySelector(".notification-arrangement")!) & Node.DOCUMENT_POSITION_FOLLOWING).toBeTruthy();
  });

  it("uses the disabled reason as the dismiss title while paused", () => {
    render(<NotificationFilterBar chips={[]} appFilter={null} onAppFilter={vi.fn()} arrangement="time" onArrangement={vi.fn()} dismissAll={{ label: "Dismiss paused", disabled: true, disabledReason: "Paused while Peppy updates your messages.", onDismissAll: vi.fn() }} />);
    const dismiss = screen.getByRole("button", { name: "Dismiss paused" });
    expect(dismiss).toHaveAttribute("title", "Paused while Peppy updates your messages.");
    expect(dismiss).toHaveAttribute("aria-describedby", "update-paused-description");
  });

  it("wraps the visible Time, App and dismiss text in toolbar labels", () => {
    const { container } = render(<NotificationFilterBar chips={[]} appFilter={null} onAppFilter={vi.fn()} arrangement="time" onArrangement={vi.fn()} dismissAll={{ label: "Dismiss up to 100", disabled: false, onDismissAll: vi.fn() }} />);
    const labels = [...container.querySelectorAll(".notification-toolbar-label")].map(label => label.textContent);
    expect(labels).toEqual(["Time", "App", "Dismiss up to 100"]);
  });
});
