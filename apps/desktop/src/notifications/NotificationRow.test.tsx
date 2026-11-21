import { cleanup, fireEvent, render, screen } from "@testing-library/react";
import { afterEach, describe, expect, it, vi } from "vitest";
import { NotificationRow, type NotificationRowProps } from "./NotificationRow";

const props: NotificationRowProps = {
  notification: {
    target: { sourceDeviceId: "phone", notificationKey: "key", lifetime: "one" },
    packageName: "chat", appName: "Chat", title: "Title", text: "Long notification body",
    postedAt: Date.now(), dismissible: true, seen: false, dismissalPending: false,
  },
  labels: { appName: n => n.appName, sourceName: () => "Phone", title: n => n.title },
  filter: { sourceDeviceId: "phone", packageName: "chat", appName: "Chat", muted: false },
  unread: true, showPhone: false, query: "",
  onDismiss: vi.fn(), onMute: vi.fn(), onCopy: vi.fn(), onAction: vi.fn(),
};

const scrollHeight = Object.getOwnPropertyDescriptor(HTMLElement.prototype, "scrollHeight");
const clientHeight = Object.getOwnPropertyDescriptor(HTMLElement.prototype, "clientHeight");

afterEach(() => {
  cleanup();
  if (scrollHeight) Object.defineProperty(HTMLElement.prototype, "scrollHeight", scrollHeight);
  else Reflect.deleteProperty(HTMLElement.prototype, "scrollHeight");
  if (clientHeight) Object.defineProperty(HTMLElement.prototype, "clientHeight", clientHeight);
  else Reflect.deleteProperty(HTMLElement.prototype, "clientHeight");
});

describe("NotificationRow", () => {
  it("disables the mute action while writes are paused", () => {
    render(<NotificationRow {...props} writesPaused />);
    expect((screen.getByRole("button", { name: "Mute Chat" }) as HTMLButtonElement).disabled).toBe(true);
  });
  it("announces unread state in the description and distinguishes read rows", () => {
    const { rerender } = render(<NotificationRow {...props} />);
    const row = screen.getByRole("article");
    expect(row.getAttribute("data-read-state")).toBe("unread");
    expect(document.getElementById(row.getAttribute("aria-describedby")!)?.textContent).toContain("Unread, ");
    rerender(<NotificationRow {...props} unread={false} />);
    expect(row.getAttribute("data-read-state")).toBe("read");
    expect(document.getElementById(row.getAttribute("aria-describedby")!)?.textContent).not.toContain("Unread, ");
  });

  it("expands and collapses overflowing text", () => {
    Object.defineProperty(HTMLElement.prototype, "scrollHeight", { configurable: true, get: () => 100 });
    Object.defineProperty(HTMLElement.prototype, "clientHeight", { configurable: true, get: () => 40 });
    render(<NotificationRow {...props} />);
    const button = screen.getByRole("button", { name: "Show more" });
    const text = document.getElementById(button.getAttribute("aria-controls")!)!;
    expect(button.getAttribute("aria-expanded")).toBe("false");
    expect(text.hasAttribute("data-clamped")).toBe(true);
    fireEvent.click(button);
    expect(button.getAttribute("aria-expanded")).toBe("true");
    expect(text.hasAttribute("data-clamped")).toBe(false);
    fireEvent.click(button);
    expect(button.getAttribute("aria-expanded")).toBe("false");
    expect(text.hasAttribute("data-clamped")).toBe(true);
  });

  it("does not clamp or show the toggle during search", () => {
    Object.defineProperty(HTMLElement.prototype, "scrollHeight", { configurable: true, get: () => 100 });
    Object.defineProperty(HTMLElement.prototype, "clientHeight", { configurable: true, get: () => 40 });
    const { container } = render(<NotificationRow {...props} query="body" />);
    expect(container.querySelector(".notif-row-text")?.hasAttribute("data-clamped")).toBe(false);
    expect(screen.queryByRole("button", { name: /Show more|Show less/ })).toBeNull();
    expect(container.querySelector("mark")?.textContent).toBe("body");
  });
});
