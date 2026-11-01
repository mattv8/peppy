import "@testing-library/jest-dom/vitest";
import { act, cleanup, fireEvent, render, renderHook, screen } from "@testing-library/react";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import type { AccountAccessResult, AccountNoticeDto } from "@peppy/desktop-ui/bridge-contracts";
import { bridge } from "./bridge";
import { formatCopy, peppyCopy } from "./generated/peppyCopy";
import { SystemRibbon, SystemRibbonAnnouncement, SystemRibbonStrip } from "./SystemRibbon";
import { accountRibbonModel, formatAccountDeadline, openAccountManageUrl, resolveSystemRibbon } from "./systemRibbonModel";
import { isAccountNoticeSnoozed, useAccountNotice } from "./useAccountNotice";

vi.mock("./bridge", { spy: true });

const NOW = Date.UTC(2026, 10, 1, 12);
const DAY_SECONDS = 86_400;
const MINUTE_MS = 60_000;
const DAY_MS = 86_400_000;

const notice = (overrides: Partial<AccountNoticeDto> = {}): AccountNoticeDto => ({
  kind: "read_only_past_due",
  severity: "error",
  deadline: null,
  manageUrl: "https://peppy.pro/account",
  dismissible: false,
  snoozeKey: "read_only_past_due:none",
  ...overrides,
});

const graceNotice = (overrides: Partial<AccountNoticeDto> = {}) => notice({
  kind: "payment_failed_grace",
  severity: "warning",
  dismissible: true,
  snoozeKey: "payment_failed_grace:none",
  ...overrides,
});

describe("resolveSystemRibbon", () => {
  it("shows server offline ahead of an account notice", () => {
    expect(resolveSystemRibbon(true, notice(), NOW)?.kind).toBe("server_offline");
  });

  it("shows the account notice when the server is reachable", () => {
    expect(resolveSystemRibbon(false, notice(), NOW)?.kind).toBe("read_only_past_due");
  });

  it("shows nothing without an offline state or notice", () => {
    expect(resolveSystemRibbon(false, null, NOW)).toBeNull();
  });

  it("hides an ending notice once its deadline has passed", () => {
    const ending = notice({ kind: "ending_scheduled", severity: "warning", deadline: NOW / 1000 - 60, dismissible: true, snoozeKey: "ending_scheduled:x" });
    expect(resolveSystemRibbon(false, ending, NOW)).toBeNull();
  });

  it("keeps an ending notice whose deadline is still ahead", () => {
    const ending = notice({ kind: "ending_scheduled", severity: "warning", deadline: NOW / 1000 + 60, dismissible: true, snoozeKey: "ending_scheduled:x" });
    expect(resolveSystemRibbon(false, ending, NOW)?.kind).toBe("ending_scheduled");
  });
});

describe("accountRibbonModel", () => {
  it("uses the no-date body when a payment grace deadline is missing", () => {
    const model = accountRibbonModel(graceNotice({ deadline: null }), NOW);
    expect(model.body).toBe(peppyCopy.account_payment_failed_grace_body_nodate);
    expect(model.actionLabel).toBe(peppyCopy.account_payment_failed_grace_action);
  });

  it("uses the no-date body when the grace deadline has passed", () => {
    const model = accountRibbonModel(graceNotice({ deadline: NOW / 1000 - 60 }), NOW);
    expect(model.body).toBe(peppyCopy.account_payment_failed_grace_body_nodate);
  });

  it("formats a future grace deadline into the body", () => {
    const deadline = NOW / 1000 + 3 * DAY_SECONDS;
    const model = accountRibbonModel(graceNotice({ deadline }), NOW);
    expect(model.body).toBe(formatCopy(peppyCopy.account_payment_failed_grace_body, formatAccountDeadline(deadline, NOW)));
  });

  it("formats an ending date into the title", () => {
    const deadline = NOW / 1000 + 5 * DAY_SECONDS;
    const model = accountRibbonModel(notice({ kind: "ending_scheduled", severity: "warning", deadline, dismissible: true, snoozeKey: "ending_scheduled:x" }), NOW);
    expect(model.title).toBe(formatCopy(peppyCopy.account_ending_scheduled_title, formatAccountDeadline(deadline, NOW)));
  });

  it("maps read-only notices to error copy with an action", () => {
    const model = accountRibbonModel(notice(), NOW);
    expect(model).toMatchObject({ kind: "read_only_past_due", severity: "error", dismissible: false, actionLabel: peppyCopy.account_read_only_past_due_action, actionUrl: "https://peppy.pro/account" });
    expect(model.title).toBe(peppyCopy.account_read_only_past_due_title);
  });

  it("drops the action when no manage URL was validated", () => {
    expect(accountRibbonModel(notice({ manageUrl: null }), NOW)).toMatchObject({ actionLabel: null, actionUrl: null });
  });
});

describe("SystemRibbon", () => {
  afterEach(cleanup);

  it("renders the offline ribbon with kind and severity hooks and no buttons", () => {
    const model = resolveSystemRibbon(true, null, NOW)!;
    const { container } = render(<SystemRibbon model={model} />);
    const ribbon = container.querySelector("#system-ribbon");
    expect(ribbon).toHaveAttribute("data-system-ribbon-kind", "server_offline");
    expect(ribbon).toHaveAttribute("data-system-ribbon-severity", "warning");
    expect(ribbon).toHaveTextContent(peppyCopy.server_offline_body);
    expect(screen.queryByRole("button")).not.toBeInTheDocument();
  });

  it("renders error severity and wires the action", () => {
    const onAction = vi.fn();
    const { container } = render(<SystemRibbon model={accountRibbonModel(notice(), NOW)} onAction={onAction} />);
    expect(container.querySelector("#system-ribbon")).toHaveAttribute("data-system-ribbon-severity", "error");
    fireEvent.click(screen.getByRole("button", { name: `${peppyCopy.account_read_only_past_due_action}, ${peppyCopy.system_banner_opens_in_browser}` }));
    expect(onAction).toHaveBeenCalledTimes(1);
  });

  it("keeps the action and dismiss controls outside the text block", () => {
    const { container } = render(<SystemRibbon model={accountRibbonModel(graceNotice(), NOW)} onAction={vi.fn()} onDismiss={vi.fn()} />);
    const text = container.querySelector(".system-ribbon > .system-ribbon-text")!;
    expect(text).toHaveAttribute("data-system-ribbon-text");
    expect(text.querySelector("strong")).toHaveTextContent(peppyCopy.account_payment_failed_grace_title);
    expect(text.querySelector(".app-notice-actions")).toBeNull();
    expect(container.querySelector(".system-ribbon > .app-notice-actions")).not.toBeNull();
  });

  it("gives the head-panel strip the full body as a tooltip", () => {
    const model = accountRibbonModel(notice(), NOW);
    const { container } = render(<SystemRibbonStrip model={model} />);
    expect(container.querySelector("#system-ribbon")).toHaveAttribute("title", model.body);
  });

  it("shows the open-failed line only when opening the manage URL failed", () => {
    const { rerender } = render(<SystemRibbon model={accountRibbonModel(notice(), NOW)} />);
    expect(screen.queryByText(peppyCopy.toggle_android_link_unavailable)).not.toBeInTheDocument();
    rerender(<SystemRibbon model={accountRibbonModel(notice(), NOW)} openFailed />);
    expect(screen.getByText(peppyCopy.toggle_android_link_unavailable)).toBeInTheDocument();
  });

  it("offers dismiss only for a dismissible notice", () => {
    const onDismiss = vi.fn();
    const { rerender } = render(<SystemRibbon model={accountRibbonModel(notice(), NOW)} onDismiss={onDismiss} />);
    expect(screen.queryByRole("button", { name: peppyCopy.system_banner_dismiss })).not.toBeInTheDocument();
    rerender(<SystemRibbon model={accountRibbonModel(graceNotice(), NOW)} onDismiss={onDismiss} />);
    fireEvent.click(screen.getByRole("button", { name: peppyCopy.system_banner_dismiss }));
    expect(onDismiss).toHaveBeenCalledTimes(1);
  });
});

describe("SystemRibbonAnnouncement", () => {
  afterEach(cleanup);

  it("announces only when the slot's kind changes and clears when it empties", () => {
    const offline = resolveSystemRibbon(true, null, NOW)!;
    const account = accountRibbonModel(notice(), NOW);
    const { container, rerender } = render(<SystemRibbonAnnouncement model={offline} />);
    const region = () => container.querySelector("#system-ribbon-announcement")!;
    const offlineText = `${peppyCopy.server_offline_title}. ${peppyCopy.server_offline_body}`;
    expect(region().textContent).toBe(offlineText);

    rerender(<SystemRibbonAnnouncement model={{ ...offline, body: "Changed in place" }} />);
    expect(region().textContent).toBe(offlineText);

    rerender(<SystemRibbonAnnouncement model={account} />);
    expect(region().textContent).toBe(`${account.title}. ${account.body}`);

    rerender(<SystemRibbonAnnouncement model={null} />);
    expect(region().textContent).toBe("");
    expect(region()).toHaveAttribute("aria-live", "polite");
  });
});

function memoryStorage(): Storage {
  const items = new Map<string, string>();
  return {
    get length() { return items.size; },
    clear: () => items.clear(),
    getItem: key => items.get(key) ?? null,
    key: index => [...items.keys()][index] ?? null,
    removeItem: key => { items.delete(key); },
    setItem: (key, value) => { items.set(key, String(value)); },
  };
}

describe("useAccountNotice", () => {
  beforeEach(() => {
    vi.stubGlobal("localStorage", memoryStorage());
    vi.useFakeTimers();
    vi.setSystemTime(NOW);
    vi.mocked(bridge.account_access!).mockReset();
  });
  afterEach(() => {
    vi.useRealTimers();
    vi.unstubAllGlobals();
    cleanup();
  });

  it("stays hidden until the first successful read", async () => {
    vi.mocked(bridge.account_access!).mockResolvedValue({ kind: "temporarily_unavailable" });
    const { result } = renderHook(() => useAccountNotice("device-a"));
    await act(async () => undefined);
    expect(result.current.notice).toBeNull();
  });

  it("drops the cached notice when the device identity changes", async () => {
    vi.mocked(bridge.account_access!).mockResolvedValue({ kind: "notice", notice: notice() });
    const { result, rerender } = renderHook(({ identity }) => useAccountNotice(identity), { initialProps: { identity: "device-a" } });
    await act(async () => undefined);
    expect(result.current.notice).toEqual(notice());

    vi.mocked(bridge.account_access!).mockResolvedValue({ kind: "temporarily_unavailable" });
    rerender({ identity: "device-b" });
    await act(async () => undefined);
    expect(result.current.notice).toBeNull();
    expect(bridge.account_access).toHaveBeenCalledTimes(2);
  });

  it("ignores a late reply that belongs to the previous identity", async () => {
    let answerOldIdentity!: (result: AccountAccessResult) => void;
    vi.mocked(bridge.account_access!)
      .mockReturnValueOnce(new Promise<AccountAccessResult>(resolve => { answerOldIdentity = resolve; }))
      .mockResolvedValueOnce({ kind: "temporarily_unavailable" });
    const { result, rerender } = renderHook(({ identity }) => useAccountNotice(identity), { initialProps: { identity: "device-a" } });
    rerender({ identity: "device-b" });
    await act(async () => undefined);
    await act(async () => { answerOldIdentity({ kind: "notice", notice: notice() }); });
    expect(result.current.notice).toBeNull();
  });

  it("keeps the last good notice through a temporary failure and clears on none", async () => {
    vi.mocked(bridge.account_access!)
      .mockResolvedValueOnce({ kind: "notice", notice: notice() })
      .mockResolvedValueOnce({ kind: "temporarily_unavailable" })
      .mockResolvedValueOnce({ kind: "none" });
    const { result } = renderHook(() => useAccountNotice("device-a"));
    await act(async () => undefined);
    expect(result.current.notice).toEqual(notice());

    vi.setSystemTime(NOW + MINUTE_MS);
    await act(async () => { window.dispatchEvent(new Event("focus")); });
    expect(result.current.notice).toEqual(notice());

    vi.setSystemTime(NOW + 2 * MINUTE_MS);
    await act(async () => { window.dispatchEvent(new Event("focus")); });
    expect(result.current.notice).toBeNull();
  });

  it("throttles focus refreshes to one per minute", async () => {
    vi.mocked(bridge.account_access!).mockResolvedValue({ kind: "none" });
    renderHook(() => useAccountNotice("device-a"));
    await act(async () => undefined);
    expect(bridge.account_access).toHaveBeenCalledTimes(1);

    await act(async () => { window.dispatchEvent(new Event("focus")); });
    expect(bridge.account_access).toHaveBeenCalledTimes(1);

    vi.setSystemTime(NOW + MINUTE_MS);
    await act(async () => { window.dispatchEvent(new Event("focus")); });
    expect(bridge.account_access).toHaveBeenCalledTimes(2);
  });

  it("snoozes a dismissible notice for 24 hours and never snoozes an error", async () => {
    vi.mocked(bridge.account_access!).mockResolvedValue({ kind: "notice", notice: graceNotice() });
    const { result, rerender } = renderHook(() => useAccountNotice("device-a"));
    await act(async () => undefined);
    act(() => result.current.dismiss());
    expect(result.current.notice).toBeNull();
    expect(isAccountNoticeSnoozed("payment_failed_grace:none", NOW)).toBe(true);

    vi.setSystemTime(NOW + DAY_MS + MINUTE_MS);
    rerender();
    expect(result.current.notice).toEqual(graceNotice());
    expect(isAccountNoticeSnoozed("payment_failed_grace:none")).toBe(false);
  });

  it("ignores dismiss for a read-only error", async () => {
    vi.mocked(bridge.account_access!).mockResolvedValue({ kind: "notice", notice: notice() });
    const { result } = renderHook(() => useAccountNotice("device-a"));
    await act(async () => undefined);
    act(() => result.current.dismiss());
    expect(result.current.notice).toEqual(notice());
    expect(isAccountNoticeSnoozed(notice().snoozeKey)).toBe(false);
  });
});

describe("openAccountManageUrl", () => {
  afterEach(() => {
    vi.restoreAllMocks();
  });

  it("opens a validated https URL through the bridge's validated command on desktop", async () => {
    const openExternal = vi.fn(async (_url: string) => undefined);
    await expect(openAccountManageUrl("https://peppy.pro/account", false, openExternal)).resolves.toBe(true);
    expect(openExternal).toHaveBeenCalledWith("https://peppy.pro/account");
  });

  it("routes desktop opens through bridge.open_external_url", async () => {
    const desktopBridge = bridge as Required<Pick<typeof bridge, "open_external_url">>;
    const openExternal = vi.spyOn(desktopBridge, "open_external_url").mockResolvedValue(undefined);
    await expect(openAccountManageUrl("https://peppy.pro/account", false, desktopBridge.open_external_url)).resolves.toBe(true);
    expect(openExternal).toHaveBeenCalledWith("https://peppy.pro/account");
  });

  it("does not open anything on desktop when the bridge has no open command", async () => {
    await expect(openAccountManageUrl("https://peppy.pro/account", false, undefined)).resolves.toBe(false);
  });

  it("opens in a new window with noopener in the browser host", async () => {
    const open = vi.spyOn(window, "open").mockReturnValue(null);
    const openExternal = vi.fn(async (_url: string) => undefined);
    await expect(openAccountManageUrl("https://peppy.pro/account", true, openExternal)).resolves.toBe(true);
    expect(open).toHaveBeenCalledWith("https://peppy.pro/account", "_blank", "noopener,noreferrer");
    expect(openExternal).not.toHaveBeenCalled();
  });

  it("refuses plain http, script URLs, and missing URLs, but allows loopback http", async () => {
    const openExternal = vi.fn(async (_url: string) => undefined);
    await expect(openAccountManageUrl("http://peppy.pro/account", false, openExternal)).resolves.toBe(false);
    await expect(openAccountManageUrl("javascript:alert(1)", false, openExternal)).resolves.toBe(false);
    await expect(openAccountManageUrl(null, false, openExternal)).resolves.toBe(false);
    vi.spyOn(window, "open").mockReturnValue(null);
    await expect(openAccountManageUrl("http://localhost:8080/account", true, openExternal)).resolves.toBe(true);
    expect(openExternal).not.toHaveBeenCalled();
  });

  it("reports false when the bridge open fails", async () => {
    const openExternal = vi.fn(async (_url: string) => { throw new Error("denied"); });
    await expect(openAccountManageUrl("https://peppy.pro/account", false, openExternal)).resolves.toBe(false);
  });
});
