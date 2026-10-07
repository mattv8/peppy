import "@testing-library/jest-dom/vitest";
import { act, cleanup, fireEvent, render, screen, waitFor } from "@testing-library/react";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { bridge, type HostedAccountView, type JoinView } from "./bridge";
import { SetupLanding } from "./SetupLanding";

const account = (overrides: Partial<HostedAccountView> = {}): HostedAccountView => ({
  available: true, signedIn: false, accountLabel: null, classification: null,
  entitlement: null, access: null, hasVault: false, resumable: false, ...overrides,
});
const join = (overrides: Partial<JoinView> = {}): JoinView => {
  const state = overrides.state ?? "waiting";
  return {
    state,
    ...(["waiting", "claimed", "confirm"].includes(state) ? { expiresInSeconds: 300 } : {}),
    ...(state === "waiting" ? { qrPayload: "secret-qr-payload" } : {}),
    ...overrides,
  };
};
const deferred = <T,>() => {
  let resolve!: (value: T) => void;
  let reject!: (reason?: unknown) => void;
  const promise = new Promise<T>((done, fail) => { resolve = done; reject = fail; });
  return { promise, resolve, reject };
};

function renderLanding(props: Partial<React.ComponentProps<typeof SetupLanding>> = {}) {
  return render(<SetupLanding mode="hosted" onMode={vi.fn()} enrolledWithoutPhone={false} pairPhone={<div>Pair phone</div>} selfHostedFallback={<div>Fallback setup</div>} onJoined={vi.fn()} {...props} />);
}

async function chooseExisting() {
  fireEvent.click(await screen.findByRole("button", { name: /already use peppy/i }));
}

function mockNextJoinPoll(next: JoinView) {
  let poll: (() => void) | undefined;
  const nativeSetInterval = window.setInterval;
  vi.spyOn(window, "setInterval").mockImplementation((handler, timeout) => {
    if (timeout === 4_000) {
      poll = handler as () => void;
      return 1 as unknown as ReturnType<typeof window.setInterval>;
    }
    return nativeSetInterval(handler, timeout) as unknown as ReturnType<typeof window.setInterval>;
  });
  vi.mocked(bridge.join_status).mockResolvedValue(next);
  return async () => {
    await waitFor(() => expect(poll).toBeDefined());
    await act(async () => poll?.());
  };
}

describe("SetupLanding", () => {
  beforeEach(() => {
    const values = new Map<string, string>();
    Object.defineProperty(window, "localStorage", { configurable: true, value: { getItem: (key: string) => values.get(key) ?? null, setItem: (key: string, value: string) => values.set(key, value), removeItem: (key: string) => values.delete(key), clear: () => values.clear() } });
    vi.restoreAllMocks();
    vi.spyOn(bridge, "hosted_account").mockResolvedValue(account());
    vi.spyOn(bridge, "join_start").mockResolvedValue(join());
    vi.spyOn(bridge, "join_status").mockResolvedValue(join());
    vi.spyOn(bridge, "join_cancel").mockResolvedValue();
    vi.spyOn(bridge, "join_confirm").mockResolvedValue(join({ state: "approved" }));
    vi.spyOn(bridge, "hosted_provision").mockResolvedValue();
    vi.spyOn(bridge, "hosted_open_billing").mockResolvedValue();
    vi.spyOn(bridge, "hosted_sign_in").mockResolvedValue(account());
    vi.spyOn(bridge, "hosted_sign_out").mockResolvedValue();
  });
  afterEach(() => {
    cleanup();
    vi.useRealTimers();
  });

  it("shows a chooser without starting a hosted join", async () => {
    renderLanding();
    await waitFor(() => expect(screen.getByRole("button", { name: /already use peppy/i })).toHaveFocus());
    expect(screen.getByRole("button", { name: /i'm new to peppy/i })).toBeInTheDocument();
    expect(bridge.join_start).not.toHaveBeenCalled();
  });

  it("uses a fixed self-hosted origin without hosted account calls or URL controls", async () => {
    vi.mocked(bridge.join_status).mockResolvedValue({ state: "idle" });
    renderLanding({ fixedOrigin: "https://community.example" });
    await waitFor(() => expect(bridge.join_start).toHaveBeenCalledWith("https://community.example"));
    expect(bridge.hosted_account).not.toHaveBeenCalled();
    expect(document.getElementById("setup-mode-select")).not.toBeInTheDocument();
    expect(document.getElementById("self-hosted-url-input")).not.toBeInTheDocument();
    expect(document.getElementById("fixed-self-hosted-origin")).toHaveTextContent("https://community.example");
    expect(await screen.findByRole("img", { name: /pairing qr/i })).toBeInTheDocument();
    expect(screen.queryByRole("link", { name: /manage account/i })).not.toBeInTheDocument();
    expect(screen.getByRole("heading", { name: /connect to your own server/i })).toBeInTheDocument();
  });

  it("shows hosted-neutral copy and a safe billing link only with account metadata", async () => {
    renderLanding({ fixedOrigin: "https://app.example.com", accountUrl: "https://account.example.com/account" });

    const link = await screen.findByRole("link", { name: /manage account & billing/i });
    expect(document.querySelector("#self-hosted-panel > h2")).toHaveTextContent("Add this computer");
    expect(screen.getByText("Scan a code with your phone to add this computer.")).toBeInTheDocument();
    expect(link).toHaveAttribute("href", "https://account.example.com/account");
    expect(link).toHaveAttribute("target", "_blank");
    expect(link).toHaveAttribute("rel", "noopener noreferrer");
    expect(document.getElementById("setup-account-billing-hint")?.compareDocumentPosition(document.getElementById("self-hosted-advanced")!)).toBe(Node.DOCUMENT_POSITION_FOLLOWING);
  });

  it("starts a hosted join only after the existing-phone choice", async () => {
    renderLanding();
    await chooseExisting();
    await waitFor(() => expect(bridge.join_start).toHaveBeenCalledWith(null));
    expect(await screen.findByRole("img", { name: /pairing qr/i })).toBeInTheDocument();
    expect(document.body).not.toHaveTextContent("secret-qr-payload");
  });

  it("shows only sign-in after the new-user choice", async () => {
    renderLanding();
    fireEvent.click(await screen.findByRole("button", { name: /i'm new to peppy/i }));
    expect(await screen.findByRole("button", { name: /continue with google/i })).toBeInTheDocument();
    expect(screen.queryByText(/subscribe/i)).not.toBeInTheDocument();
    expect(bridge.join_start).not.toHaveBeenCalled();
  });

  it("returns from either transient path to the focused chooser and cancels an existing join", async () => {
    renderLanding();
    await chooseExisting();
    await screen.findByRole("button", { name: "Back" });
    fireEvent.click(screen.getByRole("button", { name: "Back" }));
    expect(bridge.join_cancel).toHaveBeenCalledOnce();
    await waitFor(() => expect(screen.getByRole("button", { name: /already use peppy/i })).toHaveFocus());
    fireEvent.click(screen.getByRole("button", { name: /i'm new to peppy/i }));
    fireEvent.click(await screen.findByRole("button", { name: "Back" }));
    expect(await screen.findByRole("button", { name: /already use peppy/i })).toHaveFocus();
  });

  it("does not restore a hidden join when a late start resolves after Back", async () => {
    const start = deferred<JoinView>();
    vi.spyOn(bridge, "join_start").mockReturnValue(start.promise);
    renderLanding();
    await chooseExisting();
    fireEvent.click(await screen.findByRole("button", { name: "Back" }));
    await act(async () => start.resolve(join()));
    expect(screen.getByRole("button", { name: /already use peppy/i })).toBeInTheDocument();
    expect(document.getElementById("hosted-join-panel")).not.toBeInTheDocument();
  });

  it("does not restore a hidden join when a late poll resolves after Back", async () => {
    const status = deferred<JoinView>();
    let poll: (() => void) | undefined;
    vi.spyOn(window, "setInterval").mockImplementation(handler => {
      poll = handler as () => void;
      return 1 as unknown as ReturnType<typeof window.setInterval>;
    });
    vi.spyOn(bridge, "join_status").mockReturnValue(status.promise);
    renderLanding();
    await chooseExisting();
    await waitFor(() => expect(poll).toBeDefined());
    poll?.();
    fireEvent.click(screen.getByRole("button", { name: "Back" }));
    await act(async () => status.resolve(join({ state: "claimed", sas: "123456" })));
    expect(document.getElementById("hosted-join-panel")).not.toBeInTheDocument();
  });

  it("shows a retryable account loading error on the new-user path", async () => {
    vi.spyOn(bridge, "hosted_account").mockRejectedValueOnce(new Error("offline")).mockResolvedValueOnce(account());
    renderLanding();
    fireEvent.click(await screen.findByRole("button", { name: /i'm new to peppy/i }));
    const retry = await screen.findByRole("button", { name: /try again/i });
    fireEvent.click(retry);
    await waitFor(() => expect(bridge.hosted_account).toHaveBeenCalledTimes(2));
  });

  it("disables unavailable sign-in and identifies why", async () => {
    vi.spyOn(bridge, "hosted_account").mockResolvedValue(account({ available: false }));
    renderLanding();
    fireEvent.click(await screen.findByRole("button", { name: /i'm new to peppy/i }));
    expect(await screen.findByRole("button", { name: /continue with google/i })).toBeDisabled();
    expect(screen.getByRole("alert")).toBeInTheDocument();
  });

  it("keeps signed-in routing precedence and auto-joins only an existing vault", async () => {
    vi.spyOn(bridge, "hosted_account").mockResolvedValue(account({ signedIn: true, classification: "lapsed", hasVault: true, resumable: false }));
    renderLanding();
    await waitFor(() => expect(document.getElementById("hosted-lapsed-card")).toBeInTheDocument());
    expect(bridge.join_start).not.toHaveBeenCalled();
    cleanup();
    vi.spyOn(bridge, "hosted_account").mockResolvedValue(account({ signedIn: true, hasVault: true, access: "read_write" }));
    renderLanding();
    await waitFor(() => expect(bridge.join_start).toHaveBeenCalledWith(null));
    expect(document.getElementById("hosted-path-chooser")).not.toBeInTheDocument();
  });

  it("routes a newly signed-in existing-vault account to the join", async () => {
    vi.spyOn(bridge, "hosted_sign_in").mockResolvedValue(account({ signedIn: true, hasVault: true, access: "read_write" }));
    renderLanding();
    fireEvent.click(await screen.findByRole("button", { name: /i'm new to peppy/i }));
    fireEvent.click(await screen.findByRole("button", { name: /continue with google/i }));
    await waitFor(() => expect(bridge.join_start).toHaveBeenCalledWith(null));
  });

  it("does not route a deferred sign-in after Back", async () => {
    const signIn = deferred<HostedAccountView>();
    vi.spyOn(bridge, "hosted_sign_in").mockReturnValue(signIn.promise);
    renderLanding();
    fireEvent.click(await screen.findByRole("button", { name: /i'm new to peppy/i }));
    fireEvent.click(await screen.findByRole("button", { name: /continue with google/i }));
    fireEvent.click(screen.getByRole("button", { name: "Back" }));
    await act(async () => signIn.resolve(account({ signedIn: true, hasVault: true, access: "read_write" })));
    expect(screen.getByRole("button", { name: /already use peppy/i })).toBeInTheDocument();
    expect(bridge.join_start).not.toHaveBeenCalled();
  });

  it("preserves the SAS confirmation gate", async () => {
    const advanceToConfirm = mockNextJoinPoll(join({ state: "confirm", sas: "123456" }));
    renderLanding();
    await chooseExisting();
    await advanceToConfirm();
    const continueButton = await screen.findByRole("button", { name: "Continue" });
    expect(continueButton).toBeDisabled();
    fireEvent.click(screen.getByRole("checkbox"));
    await waitFor(() => expect(continueButton).toBeEnabled());
  });

  it("resets the transient hosted path when controlled mode changes", async () => {
    const view = renderLanding();
    await chooseExisting();
    view.rerender(<SetupLanding mode="self-hosted" onMode={vi.fn()} enrolledWithoutPhone={false} pairPhone={<div />} selfHostedFallback={<div />} onJoined={vi.fn()} />);
    expect(screen.getByLabelText("Server URL")).toBeInTheDocument();
    view.rerender(<SetupLanding mode="hosted" onMode={vi.fn()} enrolledWithoutPhone={false} pairPhone={<div />} selfHostedFallback={<div />} onJoined={vi.fn()} />);
    expect(await screen.findByRole("button", { name: /already use peppy/i })).toBeInTheDocument();
    expect(bridge.join_cancel).toHaveBeenCalled();
  });

  it("keeps native passphrase entry and enrolled-without-phone routing", async () => {
    vi.spyOn(bridge, "hosted_account").mockResolvedValue(account({ signedIn: true, access: "read_write" }));
    renderLanding();
    expect(await screen.findByRole("button", { name: /enter passphrase securely/i })).toBeDisabled();
    expect(document.querySelector("input[type=password]")).not.toBeInTheDocument();
    cleanup();
    renderLanding({ enrolledWithoutPhone: true, pairPhone: <div>Pair phone hero</div> });
    expect(screen.getByText("Pair phone hero")).toBeInTheDocument();
    expect(screen.queryByLabelText("Server mode")).not.toBeInTheDocument();
  });

  it("restarts expired joins and exposes retry only after failure", async () => {
    const start = vi.spyOn(bridge, "join_start").mockResolvedValue(join({ state: "expired" }));
    renderLanding();
    await chooseExisting();
    const retry = await screen.findByRole("button", { name: /try again/i });
    await waitFor(() => expect(retry).toBeEnabled());
    start.mockResolvedValue(join({ state: "waiting" }));
    fireEvent.click(retry);
    await waitFor(() => expect(bridge.join_start).toHaveBeenCalledTimes(2));
    await waitFor(() => expect(document.getElementById("hosted-join-panel")).toHaveAttribute("data-join-state", "waiting"));
    await waitFor(() => expect(screen.queryByRole("button", { name: /try again/i })).not.toBeInTheDocument());
  });

  it("shows claimed SAS without desktop approval controls", async () => {
    vi.spyOn(bridge, "join_start").mockResolvedValue(join({ state: "claimed", sas: "123456" }));
    renderLanding();
    await chooseExisting();
    expect(await screen.findByLabelText("Verification code: 123456")).toBeInTheDocument();
    expect(screen.queryByRole("button", { name: /approve|deny/i })).not.toBeInTheDocument();
  });

  it("disables confirmation cancellation while confirming and reports approval to the parent", async () => {
    const onJoined = vi.fn();
    const advanceToConfirm = mockNextJoinPoll(join({ state: "confirm", sas: "123456" }));
    const confirm = deferred<JoinView>();
    vi.spyOn(bridge, "join_confirm").mockReturnValue(confirm.promise);
    renderLanding({ onJoined });
    await chooseExisting();
    await advanceToConfirm();
    const continueButton = await screen.findByRole("button", { name: "Continue" });
    expect(continueButton).toBeDisabled();
    fireEvent.click(screen.getByRole("checkbox"));
    await waitFor(() => expect(continueButton).toBeEnabled());
    fireEvent.click(continueButton);
    expect(await screen.findByRole("button", { name: "Cancel" })).toBeDisabled();
    expect(screen.getByRole("button", { name: "Back" })).toBeDisabled();
    expect(screen.getByRole("combobox", { name: "Server mode" })).toBeDisabled();
    await act(async () => confirm.resolve(join({ state: "approved" })));
    await waitFor(() => expect(onJoined).toHaveBeenCalledOnce());
  });

  it("waits for the initial account check before offering hosted choices", async () => {
    const request = deferred<HostedAccountView>();
    vi.spyOn(bridge, "hosted_account").mockReturnValue(request.promise);
    renderLanding();
    expect(document.getElementById("hosted-path-chooser")).not.toBeInTheDocument();
    await act(async () => request.resolve(account()));
    expect(await screen.findByRole("button", { name: /already use peppy/i })).toBeEnabled();
  });

  it("turns confirmation errors into a retryable join failure", async () => {
    const advanceToConfirm = mockNextJoinPoll(join({ state: "confirm", sas: "123456" }));
    vi.spyOn(bridge, "join_confirm").mockRejectedValue(new Error("offline"));
    renderLanding();
    await chooseExisting();
    await advanceToConfirm();
    fireEvent.click(await screen.findByRole("checkbox"));
    const continueButton = await screen.findByRole("button", { name: "Continue" });
    await waitFor(() => expect(continueButton).toBeEnabled());
    fireEvent.click(continueButton);
    expect(await screen.findByRole("button", { name: /try again/i })).toBeInTheDocument();
  });

  it("retains the last countdown and announces retrying poll state", async () => {
    let poll: (() => void) | undefined;
    const nativeSetInterval = window.setInterval;
    vi.spyOn(window, "setInterval").mockImplementation((handler, timeout) => {
      if (timeout === 4_000) {
        poll = handler as () => void;
        return 1 as unknown as ReturnType<typeof window.setInterval>;
      }
      return nativeSetInterval(handler, timeout) as unknown as ReturnType<typeof window.setInterval>;
    });
    vi.spyOn(bridge, "join_status").mockResolvedValue(join({ errorCode: "join-retrying", expiresInSeconds: undefined }));
    renderLanding();
    await chooseExisting();
    await waitFor(() => expect(poll).toBeDefined());
    poll!();
    await waitFor(() => expect(bridge.join_status).toHaveBeenCalledOnce());
    await waitFor(() => expect(document.querySelector(".join-retrying-hint")).toHaveTextContent(/reconnecting/i));
    expect(document.getElementById("join-countdown")).toHaveTextContent("300s");
  });

  it("keeps billing, provisioning, recovery checkpoint, and passphrase errors on their established routes", async () => {
    vi.spyOn(bridge, "hosted_account").mockResolvedValue(account({ signedIn: true, access: "read_only" }));
    renderLanding();
    expect(await screen.findByRole("button", { name: /check subscription/i })).toBeInTheDocument();
    cleanup();
    vi.spyOn(bridge, "hosted_account").mockResolvedValue(account({ signedIn: true, access: "read_only", classification: "lapsed", hasVault: true, resumable: true }));
    renderLanding();
    await waitFor(() => expect(bridge.hosted_provision).toHaveBeenCalled());
    expect(document.getElementById("hosted-provisioning-card")).toBeInTheDocument();
    cleanup();
    vi.spyOn(bridge, "hosted_account").mockResolvedValue(account({ signedIn: true, access: "read_write" }));
    vi.spyOn(bridge, "hosted_provision").mockRejectedValue({ code: "network" });
    renderLanding();
    fireEvent.click(await screen.findByRole("checkbox"));
    fireEvent.click(screen.getByRole("button", { name: /enter passphrase securely/i }));
    expect(await screen.findByRole("alert")).toBeInTheDocument();
  });

  it("opens billing and refreshes the account when subscription actions are chosen", async () => {
    vi.spyOn(bridge, "hosted_account").mockResolvedValue(account({ signedIn: true, access: "read_only" }));
    renderLanding();
    await screen.findByRole("button", { name: /check subscription/i });
    fireEvent.click(screen.getByRole("button", { name: /subscribe at peppy.pro/i }));
    fireEvent.click(screen.getByRole("button", { name: /check subscription/i }));
    await waitFor(() => expect(bridge.hosted_account).toHaveBeenCalledTimes(2));
    expect(bridge.hosted_open_billing).toHaveBeenCalledOnce();
  });

  it("recovers a resumable vault before starting a phone join", async () => {
    vi.spyOn(bridge, "hosted_account").mockResolvedValue(account({ signedIn: true, access: "read_only", classification: "lapsed", hasVault: true, resumable: true }));
    renderLanding();
    await waitFor(() => expect(bridge.hosted_provision).toHaveBeenCalledOnce());
    expect(bridge.join_start).not.toHaveBeenCalled();
  });

  it("provisions a resumable account and an acknowledged passphrase setup", async () => {
    vi.spyOn(bridge, "hosted_account").mockResolvedValue(account({ signedIn: true, access: "read_write", resumable: true }));
    renderLanding();
    await waitFor(() => expect(bridge.hosted_provision).toHaveBeenCalledOnce());
    cleanup();
    vi.spyOn(bridge, "hosted_account").mockResolvedValue(account({ signedIn: true, access: "read_write" }));
    renderLanding();
    fireEvent.click(await screen.findByRole("checkbox"));
    fireEvent.click(screen.getByRole("button", { name: /enter passphrase securely/i }));
    await waitFor(() => expect(bridge.hosted_provision).toHaveBeenCalledTimes(2));
  });

  it("refreshes the account after a failed passphrase provision", async () => {
    vi.spyOn(bridge, "hosted_account").mockResolvedValue(account({ signedIn: true, access: "read_write" }));
    vi.spyOn(bridge, "hosted_provision").mockRejectedValue({ code: "network" });
    renderLanding();
    fireEvent.click(await screen.findByRole("checkbox"));
    fireEvent.click(screen.getByRole("button", { name: /enter passphrase securely/i }));
    await screen.findByRole("alert");
    await waitFor(() => expect(bridge.hosted_account).toHaveBeenCalledTimes(2));
  });

  it("uses the hosted origin for a fresh pairing after self-hosted setup", async () => {
    const view = renderLanding({ mode: "self-hosted" });
    fireEvent.change(screen.getByLabelText("Server URL"), { target: { value: "https://self.example" } });
    fireEvent.click(screen.getByRole("button", { name: "Connect" }));
    await waitFor(() => expect(bridge.join_start).toHaveBeenCalledWith("https://self.example"));
    view.rerender(<SetupLanding mode="hosted" onMode={vi.fn()} enrolledWithoutPhone={false} pairPhone={<div />} selfHostedFallback={<div />} onJoined={vi.fn()} />);
    fireEvent.click(await screen.findByRole("button", { name: /already use peppy/i }));
    await waitFor(() => expect(bridge.join_start).toHaveBeenLastCalledWith(null));
  });

  it("announces the confirmation state through the dedicated status region", async () => {
    const advanceToConfirm = mockNextJoinPoll(join({ state: "confirm", sas: "123456" }));
    renderLanding();
    await chooseExisting();
    await advanceToConfirm();
    await waitFor(() => expect(document.getElementById("setup-status-region")).toHaveTextContent(/continue only if/i));
  });

  it("only connects self-hosted HTTPS origins and always offers advanced setup", async () => {
    renderLanding({ mode: "self-hosted" });
    const input = screen.getByLabelText("Server URL");
    const connect = screen.getByRole("button", { name: "Connect" });
    fireEvent.change(input, { target: { value: "http://example.test" } });
    expect(connect).toBeDisabled();
    fireEvent.change(input, { target: { value: "https://example.test" } });
    fireEvent.click(connect);
    await waitFor(() => expect(bridge.join_start).toHaveBeenCalledWith("https://example.test"));
    expect(document.querySelector("#self-hosted-advanced")).toHaveTextContent("Fallback setup");
  });

  it("persists the selected server mode and notifies the parent", () => {
    const onMode = vi.fn();
    renderLanding({ onMode });
    fireEvent.change(screen.getByRole("combobox", { name: "Server mode" }), { target: { value: "self-hosted" } });
    expect(onMode).toHaveBeenCalledWith("self-hosted");
    expect(window.localStorage.getItem("peppy.setup.mode")).toBe("self-hosted");
  });

  it("keeps the setup heading while routing enrolled desktops to phone pairing", () => {
    renderLanding({ enrolledWithoutPhone: true, pairPhone: <div>Pair phone hero</div> });
    expect(screen.getByRole("heading", { level: 1 })).toHaveTextContent("Set up Peppy");
    expect(screen.queryByLabelText("Server mode")).not.toBeInTheDocument();
  });

  it("shows a retryable sign-in error", async () => {
    vi.spyOn(bridge, "hosted_sign_in").mockRejectedValueOnce(new Error("offline")).mockResolvedValueOnce(account());
    renderLanding();
    fireEvent.click(await screen.findByRole("button", { name: /i'm new to peppy/i }));
    fireEvent.click(await screen.findByRole("button", { name: /continue with google/i }));
    fireEvent.click(await screen.findByRole("button", { name: /try again/i }));
    await waitFor(() => expect(bridge.hosted_sign_in).toHaveBeenCalledTimes(2));
  });

  it("accepts a fresh hosted account after a self-hosted mode round trip", async () => {
    const fresh = account({ signedIn: true, hasVault: true, access: "read_write" });
    const request = deferred<HostedAccountView>();
    vi.spyOn(bridge, "hosted_account").mockReturnValue(request.promise);
    const view = renderLanding({ mode: "self-hosted" });
    view.rerender(<SetupLanding mode="hosted" onMode={vi.fn()} enrolledWithoutPhone={false} pairPhone={<div />} selfHostedFallback={<div />} onJoined={vi.fn()} />);
    await waitFor(() => expect(bridge.hosted_account).toHaveBeenCalled());
    await act(async () => request.resolve(fresh));
    await waitFor(() => expect(bridge.join_start).toHaveBeenCalledWith(null));
  });

  it("cancels an active self-hosted join when unmounted", async () => {
    const view = renderLanding({ mode: "self-hosted" });
    fireEvent.change(screen.getByLabelText("Server URL"), { target: { value: "https://self.example" } });
    fireEvent.click(screen.getByRole("button", { name: "Connect" }));
    await waitFor(() => expect(bridge.join_start).toHaveBeenCalled());
    view.unmount();
    expect(bridge.join_cancel).toHaveBeenCalled();
  });

  it("does not route on account check during billing refresh", async () => {
    const request = deferred<HostedAccountView>();
    vi.spyOn(bridge, "hosted_account").mockReturnValue(request.promise);
    renderLanding();
    // Resolve to billing state
    await act(async () => request.resolve(account({ signedIn: true, access: "read_only" })));
    await screen.findByRole("button", { name: /check subscription/i });
    expect(document.getElementById("hosted-billing-card")).toBeInTheDocument();
    // Start a new account fetch via focus (billing route listens to focus)
    const refreshRequest = deferred<HostedAccountView>();
    vi.spyOn(bridge, "hosted_account").mockReturnValueOnce(refreshRequest.promise);
    act(() => { window.dispatchEvent(new Event("focus")); });
    // Billing card should remain visible while refresh is pending
    expect(document.getElementById("hosted-billing-card")).toBeInTheDocument();
    // Resolve refresh - billing card should still be there (no route change)
    await act(async () => refreshRequest.resolve(account({ signedIn: true, access: "read_only" })));
    expect(document.getElementById("hosted-billing-card")).toBeInTheDocument();
  });

  it("restarts expired joins without a preceding cancel and restores QR and active state", async () => {
    const start = vi.spyOn(bridge, "join_start").mockResolvedValue(join({ state: "expired" }));
    const cancel = vi.spyOn(bridge, "join_cancel").mockResolvedValue();
    renderLanding();
    await chooseExisting();
    // First start should happen
    await waitFor(() => expect(start).toHaveBeenCalledTimes(1));
    // Retry on expired
    const retry = await screen.findByRole("button", { name: /try again/i });
    await waitFor(() => expect(retry).toBeEnabled());
    start.mockResolvedValue(join({ state: "waiting" }));
    fireEvent.click(retry);
    // Second start should be called without a cancel in between
    await waitFor(() => expect(start).toHaveBeenCalledTimes(2));
    // Cancel should not have been called between starts
    expect(cancel).not.toHaveBeenCalled();
    // QR should return and active state should be restored
    await waitFor(() => expect(document.getElementById("join-qr-image")).toBeInTheDocument());
    expect(document.getElementById("hosted-join-panel")).toHaveAttribute("data-join-state", "waiting");
  });

  it("stops a hidden join when enrolledWithoutPhone changes to true", async () => {
    const status = deferred<JoinView>();
    let poll: (() => void) | undefined;
    vi.spyOn(window, "setInterval").mockImplementation(handler => {
      poll = handler as () => void;
      return 1 as unknown as ReturnType<typeof window.setInterval>;
    });
    vi.spyOn(bridge, "join_status").mockReturnValue(status.promise);
    const view = renderLanding();
    await chooseExisting();
    await waitFor(() => expect(poll).toBeDefined());
    // Rerender with enrolledWithoutPhone=true (triggers pairPhone path)
    const cancel = vi.spyOn(bridge, "join_cancel").mockResolvedValue();
    view.rerender(<SetupLanding mode="hosted" onMode={vi.fn()} enrolledWithoutPhone={true} pairPhone={<div>Pair phone hero</div>} selfHostedFallback={<div />} onJoined={vi.fn()} />);
    // Should have canceled the active join
    expect(cancel).toHaveBeenCalled();
    // Poll handler should not restore the join after enrollment change
    poll?.();
    await act(async () => status.resolve(join({ state: "claimed", sas: "123456" })));
    expect(document.getElementById("hosted-join-panel")).not.toBeInTheDocument();
    expect(screen.getByText("Pair phone hero")).toBeInTheDocument();
  });
});
