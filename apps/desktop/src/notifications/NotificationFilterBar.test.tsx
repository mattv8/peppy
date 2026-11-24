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

  it("wraps the visible Time, App and dismiss text in toolbar labels", () => {
    const { container } = render(<NotificationFilterBar chips={[]} appFilter={null} onAppFilter={vi.fn()} arrangement="time" onArrangement={vi.fn()} dismissAll={{ label: "Dismiss up to 100", disabled: false, onDismissAll: vi.fn() }} />);
    const labels = [...container.querySelectorAll(".notification-toolbar-label")].map(label => label.textContent);
    expect(labels).toEqual(["Time", "App", "Dismiss up to 100"]);
  });
});
