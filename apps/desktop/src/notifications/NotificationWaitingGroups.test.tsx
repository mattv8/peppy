import "@testing-library/jest-dom/vitest";
import { cleanup, fireEvent, render, screen } from "@testing-library/react";
import { afterEach, describe, expect, it, vi } from "vitest";
import type { MirroredNotification } from "../bridge/bridge";
import { NotificationWaitingGroups, type WaitingGroup, type WaitingGroupRowProps } from "./NotificationWaitingGroups";

const notification = (index: number, phoneKey: string): MirroredNotification => ({
  target: { sourceDeviceId: phoneKey, notificationKey: `key-${index}`, lifetime: "one" },
  packageName: "chat", appName: "Chat", title: `Title ${index}`, text: "Body", postedAt: Date.now(),
  dismissible: true, seen: true, dismissalPending: true,
});

const group = (phoneKey: string, count: number): WaitingGroup => ({
  phoneKey, phoneName: "Pixel", notifications: Array.from({ length: count }, (_, index) => notification(index, phoneKey)),
});

const rowProps: WaitingGroupRowProps = {
  labels: { appName: n => n.appName, sourceName: () => "Pixel", title: n => n.title },
  filtersByApp: new Map(),
  showPhone: false, query: "", writesPaused: false,
  onDismiss: vi.fn(), onMute: vi.fn(), onCopy: vi.fn(), onAction: vi.fn(),
};

afterEach(() => cleanup());

describe("NotificationWaitingGroups", () => {
  it("keeps its wrapper in the page without groups, so its toggle target resolves", () => {
    const { container } = render(<NotificationWaitingGroups source="count" groups={[]} rowProps={rowProps} />);
    const wrapper = container.querySelector("#notifications-waiting-groups");
    expect(wrapper).toHaveAttribute("data-waiting-groups", "count");
    expect(wrapper?.children).toHaveLength(0);
  });

  it("renders an empty placeholder for each closed phone reveal", () => {
    const { container } = render(<NotificationWaitingGroups source="blocker" groups={[]} rowProps={rowProps} closedPhoneKeys={["p1"]} />);
    const placeholder = container.querySelector("#notifications-hidden-p1");
    expect(placeholder).toBeInTheDocument();
    expect(placeholder?.children).toHaveLength(0);
    expect(container.querySelector("#notifications-blocker-groups")).toContainElement(placeholder as HTMLElement);
  });

  it("shows the first three locked rows per phone and expands the rest", () => {
    const { container } = render(<NotificationWaitingGroups source="count" groups={[group("p1", 5)]} rowProps={rowProps} />);
    const section = container.querySelector("#notifications-hidden-p1")!;
    expect(container.querySelector("#notifications-waiting-groups")).toHaveAttribute("data-waiting-groups", "count");
    expect(section).toHaveAttribute("data-hidden-group");
    expect(section).toHaveAttribute("data-hidden-source", "count");
    expect(section.querySelector("strong")).toHaveTextContent("Dismissed, waiting for Pixel");
    expect(section.textContent).toContain("You can't undo or cancel these. Pixel hasn't cleared them yet.");
    expect(section.querySelectorAll("article")).toHaveLength(3);

    const more = screen.getByRole("button", { name: "Show 2 more" });
    expect(more).toHaveAttribute("aria-expanded", "false");
    fireEvent.click(more);
    expect(section.querySelectorAll("article")).toHaveLength(5);
    expect(screen.getByRole("button", { name: "Show fewer" })).toHaveAttribute("aria-expanded", "true");
  });

  it("keeps each row locked with a waiting status and Mute only", () => {
    const { container } = render(<NotificationWaitingGroups source="blocker" groups={[group("p1", 1)]} rowProps={rowProps} />);
    const row = container.querySelector("article")!;
    expect(row.getAttribute("data-dismiss-state")).toBe("pending");
    expect(row.querySelector("[data-row-status='waiting']")?.textContent).toBe("Dismissed, waiting for Pixel");
    expect(screen.queryByRole("button", { name: /^Dismiss / })).toBeNull();
    expect(screen.getByRole("button", { name: "Mute Chat" })).toBeTruthy();
    expect(container.querySelector("#notifications-blocker-groups")).toHaveAttribute("data-waiting-groups", "blocker");
    expect(container.querySelector("#notifications-hidden-p1")).toHaveAttribute("data-hidden-source", "blocker");
    expect(container.textContent).toContain("You can't undo or cancel these. Pixel will clear them once it can.");
  });

  it("shows dismissed rows as read even when they were new", () => {
    const { container } = render(<NotificationWaitingGroups source="count" groups={[group("p1", 1)]} rowProps={rowProps} />);
    expect(container.querySelector("article")).toHaveAttribute("data-read-state", "read");
    expect(container.textContent).not.toContain("Unread");
  });
});
