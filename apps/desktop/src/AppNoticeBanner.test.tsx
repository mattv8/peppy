import "@testing-library/jest-dom/vitest";
import { cleanup, fireEvent, render, screen } from "@testing-library/react";
import { afterEach, describe, expect, it, vi } from "vitest";
import { AppNoticeBanner } from "./AppNoticeBanner";

afterEach(cleanup);

describe("AppNoticeBanner", () => {
  it("hides when there are no notices", () => {
    const { container } = render(<AppNoticeBanner notices={[]} />);

    expect(container).toBeEmptyDOMElement();
  });

  it("binds notices once per source with refresh first", () => {
    render(<AppNoticeBanner notices={[
      { source: "action", severity: "info", message: "Action complete" },
      { source: "refresh", severity: "error", message: "Refresh failed" },
      { source: "action", severity: "warning", message: "Later action" },
    ]} />);

    const rows = screen.getAllByTestId(/app-notice-/);
    expect(rows.map(row => row.dataset.appNoticeSource)).toEqual(["refresh", "action"]);
    expect(rows[0]).toHaveTextContent("Refresh failed");
    expect(rows[1]).toHaveTextContent("Action complete");
  });

  it("does not offer dismissal for persistent notices", () => {
    const onDismiss = vi.fn();
    render(<AppNoticeBanner notices={[
      { source: "refresh", severity: "error", message: "Storage unavailable", persistent: true },
    ]} onDismiss={onDismiss} />);

    expect(screen.queryByRole("button", { name: /dismiss/i })).not.toBeInTheDocument();
    expect(onDismiss).not.toHaveBeenCalled();
  });

  it("retries only a refresh notice and reflects busy state", () => {
    const onRetry = vi.fn();
    render(<AppNoticeBanner notices={[
      { source: "refresh", severity: "error", message: "Refresh failed" },
      { source: "action", severity: "error", message: "Action failed" },
    ]} onRetry={onRetry} retrying />);

    const retry = screen.getByRole("button", { name: "Retrying refresh" });
    expect(retry).toBeDisabled();
    fireEvent.click(retry);
    expect(onRetry).not.toHaveBeenCalled();
  });

  it("retries refresh with no event argument", () => {
    const onRetry = vi.fn();
    render(<AppNoticeBanner notices={[
      { source: "refresh", severity: "error", message: "Refresh failed" },
      { source: "action", severity: "error", message: "Action failed" },
    ]} onRetry={onRetry} />);

    fireEvent.click(screen.getByRole("button", { name: "Retry refresh" }));
    expect(onRetry).toHaveBeenCalledWith();
    expect(screen.getAllByRole("button", { name: "Retry refresh" })).toHaveLength(1);
  });

  it("uses one live region per row with severity-appropriate announcements", () => {
    const { container } = render(<AppNoticeBanner notices={[
      { source: "refresh", severity: "error", message: "Refresh failed" },
      { source: "action", severity: "info", message: "Action complete" },
    ]} />);

    expect(screen.getByRole("alert")).toHaveAttribute("data-app-notice-message-source", "refresh");
    expect(screen.getByRole("status")).toHaveAttribute("data-app-notice-message-source", "action");
    expect(container.querySelectorAll('[role="alert"], [role="status"]')).toHaveLength(2);
  });

  it("keeps a notice live region mounted for identical notice updates", () => {
    const notices = [{ source: "refresh" as const, severity: "error" as const, message: "Refresh failed" }];
    const { rerender } = render(<AppNoticeBanner notices={notices} />);
    const announcement = screen.getByRole("alert");

    rerender(<AppNoticeBanner notices={[...notices]} />);

    expect(screen.getByRole("alert")).toBe(announcement);
  });

  it("replaces the live message when a notice severity changes", () => {
    const notice = { source: "refresh" as const, severity: "warning" as const, message: "Refresh failed" };
    const { rerender } = render(<AppNoticeBanner notices={[notice]} />);
    const announcement = screen.getByRole("status");

    rerender(<AppNoticeBanner notices={[{ ...notice, severity: "error" }]} />);

    expect(screen.getByRole("alert")).not.toBe(announcement);
  });

  it("dismisses the selected source", () => {
    const onDismiss = vi.fn();
    render(<AppNoticeBanner notices={[
      { source: "refresh", severity: "warning", message: "Refresh warning" },
      { source: "action", severity: "info", message: "Action complete" },
    ]} onDismiss={onDismiss} />);

    fireEvent.click(screen.getByRole("button", { name: "Dismiss action notice" }));
    expect(onDismiss).toHaveBeenCalledWith("action");
  });
});
