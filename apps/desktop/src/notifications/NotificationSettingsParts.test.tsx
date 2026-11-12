import "@testing-library/jest-dom/vitest";
import { cleanup, fireEvent, render, screen } from "@testing-library/react";
import { afterEach, expect, it } from "vitest";
import { NotificationSettingsHelp } from "./NotificationSettingsParts";

afterEach(cleanup);

it("returns focus to the notification help trigger after closing", () => {
  render(<NotificationSettingsHelp id="notification-sounds-help" title="Notification sounds">
    <p>Sound follows this computer&apos;s system settings.</p>
  </NotificationSettingsHelp>);

  const trigger = screen.getByRole("button", { name: "About notification sounds" });
  fireEvent.click(trigger);

  expect(screen.getByRole("dialog", { name: "Notification sounds" })).toBeVisible();
  fireEvent.click(screen.getByRole("button", { name: "Close" }));

  expect(trigger).toHaveFocus();
});

it("closes notification help with Escape and keeps tab focus in the dialog", () => {
  render(<NotificationSettingsHelp id="notification-preview-help" title="Banner preview">
    <p>Choose how much content appears in a banner.</p>
  </NotificationSettingsHelp>);

  const trigger = screen.getByRole("button", { name: "About banner preview" });
  fireEvent.click(trigger);
  const dialog = screen.getByRole("dialog", { name: "Banner preview" });
  const close = screen.getByRole("button", { name: "Close" });
  close.focus();
  fireEvent.keyDown(document, { key: "Tab" });

  expect(dialog.contains(document.activeElement)).toBe(true);
  fireEvent.keyDown(document, { key: "Escape" });
  expect(trigger).toHaveFocus();
});
