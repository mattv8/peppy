import "@testing-library/jest-dom/vitest";
import { cleanup, fireEvent, render, screen } from "@testing-library/react";
import { afterEach, describe, expect, it } from "vitest";
import { NotificationDismissHelp } from "./NotificationDismissHelp";

afterEach(() => cleanup());

describe("NotificationDismissHelp", () => {
  it("opens a titled dialog with the blocker steps and closes back to the trigger", () => {
    const { container } = render(<NotificationDismissHelp triggerId="phone-blocker-p1-help" triggerLabel="About Notification access is off on Pixel" help={{ kind: "blocker", reason: "notificationAccessOff", phoneName: "Pixel" }} />);
    const trigger = container.querySelector("#phone-blocker-p1-help") as HTMLButtonElement;
    fireEvent.click(trigger);
    expect(screen.getByRole("heading", { name: "Turn on notification access" })).toBeInTheDocument();
    expect(document.body.textContent).toContain("On Pixel, open Settings.");
    expect(document.body.textContent).toContain("Pixel will then try to clear the dismissed notifications.");
    fireEvent.click(screen.getByRole("button", { name: "Close" }));
    expect(screen.queryByRole("heading", { name: "Turn on notification access" })).toBeNull();
    expect(document.activeElement).toBe(trigger);
  });

  it("explains waiting dismissals with the count", () => {
    render(<NotificationDismissHelp triggerId="notifications-pending-help" triggerLabel="About notifications waiting for phone" help={{ kind: "waiting", count: 2 }} />);
    fireEvent.click(screen.getByRole("button", { name: "About notifications waiting for phone" }));
    expect(screen.getByRole("heading", { name: "Waiting for your phone" })).toBeInTheDocument();
    expect(document.body.textContent).toContain("Choose \u201C2 waiting for phone\u201D to see them.");
  });
});
