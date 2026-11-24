import "@testing-library/jest-dom/vitest";
import { cleanup, fireEvent, render, screen } from "@testing-library/react";
import { afterEach, describe, expect, it, vi } from "vitest";
import { NotificationWaitingControl } from "./NotificationWaitingControl";

afterEach(() => cleanup());

describe("NotificationWaitingControl", () => {
  it("renders nothing at zero", () => {
    const { container } = render(<NotificationWaitingControl count={0} expanded={false} onToggle={vi.fn()} />);
    expect(container.querySelector("#notifications-pending-count")).toBeNull();
  });

  it("shows the count with a collapsed toggle that controls the waiting groups", () => {
    const { container } = render(<NotificationWaitingControl count={3} expanded={false} onToggle={vi.fn()} />);
    const group = container.querySelector("#notifications-pending-count")!;
    expect(group).toHaveAttribute("role", "group");
    expect(group).toHaveAttribute("data-pending-count", "3");
    expect(group).toHaveAttribute("data-pending-expanded", "false");
    const toggle = container.querySelector("#notifications-pending-toggle")!;
    expect(toggle).toHaveAttribute("aria-expanded", "false");
    expect(toggle).toHaveAttribute("aria-controls", "notifications-waiting-groups");
    expect(toggle).toHaveTextContent("3 waiting for phone");
  });

  it("keeps the same text when expanded and reports the toggle", () => {
    const onToggle = vi.fn();
    const { container } = render(<NotificationWaitingControl count={1} expanded onToggle={onToggle} />);
    expect(container.querySelector("#notifications-pending-count")).toHaveAttribute("data-pending-expanded", "true");
    expect(container.querySelector(".notification-pending-chevron")).toHaveAttribute("data-expanded");
    expect(container.querySelector(".notification-pending-count-number")).toHaveTextContent("1");
    const toggle = screen.getByRole("button", { name: "1 waiting for phone" });
    expect(toggle).toHaveAttribute("aria-expanded", "true");
    fireEvent.click(toggle);
    expect(onToggle).toHaveBeenCalledOnce();
  });

  it("opens an explained help dialog from the (i) button", () => {
    const { container } = render(<NotificationWaitingControl count={2} expanded={false} onToggle={vi.fn()} />);
    const help = container.querySelector("#notifications-pending-help") as HTMLButtonElement;
    expect(help).toHaveAttribute("aria-label", "About notifications waiting for phone");
    fireEvent.click(help);
    expect(document.body.textContent).toContain("Waiting for your phone");
    expect(document.body.textContent).toContain("Choose \u201C2 waiting for phone\u201D to see them.");
  });
});
