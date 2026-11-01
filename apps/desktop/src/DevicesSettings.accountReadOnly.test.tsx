import "@testing-library/jest-dom/vitest";
import { cleanup, render, screen, waitFor } from "@testing-library/react";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { DevicesSettings } from "./DevicesSettings";
import { bridge, type DeviceRosterView } from "./bridge";
import { peppyCopy } from "./generated/peppyCopy";

vi.mock("./bridge", { spy: true });

type PresenceState = "connected" | "delayed" | "never";
type DeviceRole = "gateway" | "desktop";

const roster = (role: DeviceRole, state: PresenceState): DeviceRosterView => ({
  currentDeviceId: "self",
  devices: [
    { id: "self", name: "This Mac", kind: "desktop", role: "owner", revoked: false },
    {
      id: "phone",
      name: "Pixel 8",
      kind: role === "gateway" ? "android" : "desktop",
      role: role === "gateway" ? "gateway" : "owner",
      revoked: false,
      presence: state === "delayed"
        ? { state: "seen", last_seen_age_seconds: 300 }
        : { state, last_seen_age_seconds: state === "connected" ? 0 : null },
    },
  ],
});

const presenceBadge = (container: HTMLElement) => container.querySelector('[data-device-id="phone"] [data-device-badge="presence"]');

describe("DevicesSettings under a read-only account", () => {
  beforeEach(() => {
    vi.clearAllMocks();
  });
  afterEach(cleanup);

  it.each([
    ["gateway", "connected", false, "Connected", "connected", false],
    ["gateway", "delayed", false, "Last seen 5m ago", "delayed", false],
    ["gateway", "never", false, "Never connected", "never", false],
    ["desktop", "connected", false, "Connected", "connected", false],
    ["desktop", "delayed", false, "Last seen 5m ago", "delayed", false],
    ["desktop", "never", false, "Never connected", "never", false],
    ["gateway", "connected", true, "Connected", "connected", true],
    ["gateway", "delayed", true, "Last seen 5m ago", "paused", true],
    ["gateway", "never", true, "Never connected", "never", true],
    ["desktop", "connected", true, "Connected", "connected", false],
    ["desktop", "delayed", true, "Last seen 5m ago", "paused", false],
    ["desktop", "never", true, "Never connected", "never", false],
  ] as const)(
    "%s %s with read-only=%s shows %s as %s (paused badge %s)",
    async (role, state, readOnly, label, presence, badge) => {
      vi.mocked(bridge.list_devices).mockResolvedValue(roster(role, state));
      const { container } = render(<DevicesSettings accountReadOnly={readOnly} />);
      await waitFor(() => expect(presenceBadge(container)).toBeInTheDocument());

      const presenceNode = presenceBadge(container)!;
      expect(presenceNode).toHaveTextContent(label);
      expect(presenceNode).toHaveAttribute("data-presence-state", presence);
      const pausedBadge = container.querySelector('[data-device-id="phone"] [data-device-badge="account-paused"]');
      expect(Boolean(pausedBadge)).toBe(badge);
      if (badge) expect(pausedBadge).toHaveTextContent(peppyCopy.devices_sending_paused_badge);
      expect(Boolean(container.querySelector("[data-devices-read-only-caption]"))).toBe(readOnly);
    },
  );

  it("shows one caption above the list while the account is read-only", async () => {
    vi.mocked(bridge.list_devices).mockResolvedValue(roster("gateway", "delayed"));
    const { container } = render(<DevicesSettings accountReadOnly />);
    const caption = await waitFor(() => {
      const node = container.querySelector("[data-devices-read-only-caption]");
      expect(node).toBeInTheDocument();
      return node!;
    });
    expect(caption).toHaveTextContent(peppyCopy.devices_read_only_caption);
    expect(container.querySelectorAll("[data-devices-read-only-caption]")).toHaveLength(1);
  });

  it("keeps server offline precedence: no paused wording while offline", async () => {
    vi.mocked(bridge.list_devices).mockResolvedValue(roster("gateway", "delayed"));
    const { container } = render(<DevicesSettings serverOffline accountReadOnly />);
    await waitFor(() => expect(presenceBadge(container)).toBeInTheDocument());
    expect(presenceBadge(container)).toHaveAttribute("data-presence-state", "delayed");
    expect(container.querySelector('[data-device-badge="account-paused"]')).not.toBeInTheDocument();
    expect(container.querySelector("[data-devices-read-only-caption]")).not.toBeInTheDocument();
    expect(screen.getByText(/Server offline/)).toBeInTheDocument();
  });
});
