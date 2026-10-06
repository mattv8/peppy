import "@testing-library/jest-dom/vitest";
import { cleanup, fireEvent, render, screen, waitFor } from "@testing-library/react";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { bridge, type HostedAccountView, type JoinView } from "./bridge";
import { SetupLanding } from "./SetupLanding";

const account = (overrides: Partial<HostedAccountView> = {}): HostedAccountView => ({
  available: true, signedIn: false, accountLabel: null, classification: null,
  entitlement: null, access: null, hasVault: false, resumable: false, ...overrides,
});
const join = (overrides: Partial<JoinView> = {}): JoinView => ({ state: "waiting", qrPayload: "secret-qr-payload", expiresInSeconds: 300, ...overrides });

function renderLanding(props: Partial<React.ComponentProps<typeof SetupLanding>> = {}) {
  return render(<SetupLanding mode="hosted" onMode={vi.fn()} enrolledWithoutPhone={false} pairPhone={<div>Pair phone</div>} selfHostedFallback={<div>Fallback setup</div>} onJoined={vi.fn()} {...props} />);
}

describe("SetupLanding", () => {
  beforeEach(() => {
    const values = new Map<string, string>();
    Object.defineProperty(window, "localStorage", { configurable: true, value: { getItem: (key: string) => values.get(key) ?? null, setItem: (key: string, value: string) => values.set(key, value), clear: () => values.clear() } });
    window.localStorage.clear();
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
  afterEach(cleanup);

  it("defaults to hosted and starts a hosted join", async () => {
    renderLanding();
    await waitFor(() => expect(bridge.join_start).toHaveBeenCalledWith(null));
    expect(screen.getByLabelText("Server mode")).toHaveValue("hosted");
  });

  it("switches mode and shows URL input", async () => {
    const onMode = vi.fn();
    const view = renderLanding({ onMode });
    fireEvent.change(screen.getByLabelText("Server mode"), { target: { value: "self-hosted" } });
    expect(onMode).toHaveBeenCalledWith("self-hosted");
    view.rerender(<SetupLanding mode="self-hosted" onMode={onMode} enrolledWithoutPhone={false} pairPhone={<div>Pair phone</div>} selfHostedFallback={<div>Fallback setup</div>} onJoined={vi.fn()} />);
    expect(screen.getByLabelText("Server URL")).toBeInTheDocument();
  });

  it("renders a QR image without exposing its payload", async () => {
    renderLanding();
    await waitFor(() => expect(document.getElementById("join-qr-image")).toBeInTheDocument());
    expect(document.body).not.toHaveTextContent("secret-qr-payload");
  });

  it("restarts an expired join", async () => {
    vi.spyOn(bridge, "join_start").mockResolvedValue(join({ state: "expired" }));
    renderLanding();
    const retry = await screen.findByRole("button", { name: /try again/i });
    await waitFor(() => expect(retry).toBeEnabled());
    fireEvent.click(retry);
    await waitFor(() => expect(bridge.join_start).toHaveBeenCalledTimes(2));
  });

  it("shows claimed SAS without desktop approval controls", async () => {
    vi.spyOn(bridge, "join_start").mockResolvedValue(join({ state: "claimed", sas: "123456" }));
    renderLanding();
    expect(await screen.findByLabelText("Verification code: 123456")).toBeInTheDocument();
    expect(screen.queryByRole("button", { name: /approve|deny/i })).not.toBeInTheDocument();
  });

  it("requires verification before confirming a join", async () => {
    vi.spyOn(bridge, "join_start").mockResolvedValue(join({ state: "confirm", sas: "123456" }));
    renderLanding();
    const continueButton = await screen.findByRole("button", { name: "Continue" });
    expect(continueButton).toBeDisabled();
    fireEvent.click(screen.getByRole("checkbox"));
    // The QR is still rendering right after start; Continue enables once that settles.
    await waitFor(() => expect(continueButton).toBeEnabled());
    fireEvent.click(continueButton);
    await waitFor(() => expect(bridge.join_confirm).toHaveBeenCalledOnce());
  });

  it("disables cancel button while confirming join", async () => {
    vi.spyOn(bridge, "join_start").mockResolvedValue(join({ state: "confirm", sas: "123456" }));
    let finishConfirm!: (view: JoinView) => void;
    vi.spyOn(bridge, "join_confirm").mockReturnValue(new Promise(resolve => { finishConfirm = resolve; }));
    renderLanding();
    const continueButton = await screen.findByRole("button", { name: "Continue" });
    fireEvent.click(screen.getByRole("checkbox"));
    await waitFor(() => expect(continueButton).toBeEnabled());
    expect(screen.getByRole("button", { name: "Cancel" })).toBeEnabled();
    fireEvent.click(continueButton);
    await waitFor(() => expect(screen.getByRole("button", { name: "Cancel" })).toBeDisabled());
    finishConfirm(join({ state: "approved" }));
  });

  it("announces confirm state distinctly", async () => {
    vi.spyOn(bridge, "join_start").mockResolvedValue(join({ state: "confirm", sas: "123456" }));
    renderLanding();
    await waitFor(() => expect(document.getElementById("setup-status-region")).toHaveTextContent(/Continue only if/i));
  });

  it("shows retry button only on error", async () => {
    vi.spyOn(bridge, "join_start").mockResolvedValue(join({ state: "failed" }));
    renderLanding();
    const retryButton = await screen.findByRole("button", { name: /try again/i });
    expect(retryButton).toBeInTheDocument();
    await waitFor(() => expect(retryButton).toBeEnabled());

    // After success, retry button disappears
    vi.spyOn(bridge, "join_start").mockResolvedValue(join({ state: "waiting" }));
    fireEvent.click(retryButton);
    await waitFor(() => expect(screen.queryByRole("button", { name: /try again/i })).not.toBeInTheDocument());
  });

  it("shows join-retrying hint when errorCode is join-retrying", async () => {
    vi.spyOn(bridge, "join_start").mockResolvedValue(join());
    vi.spyOn(bridge, "join_status").mockResolvedValue(join({ errorCode: "join-retrying" }));
    renderLanding();
    await waitFor(() => expect(document.querySelector(".join-retrying-hint")).toHaveTextContent(/Reconnecting/i), { timeout: 5000 });
  });

  it("keeps h1 and hides selector for enrolledWithoutPhone", () => {
    renderLanding({ enrolledWithoutPhone: true, pairPhone: <div>Pair phone hero</div> });
    expect(screen.getByRole("heading", { level: 1 })).toHaveTextContent("Set up Peppy");
    expect(screen.queryByLabelText("Server mode")).not.toBeInTheDocument();
    expect(screen.getByText("Pair phone hero")).toBeInTheDocument();
  });

  it("notifies the parent after approval", async () => {
    const onJoined = vi.fn();
    vi.spyOn(bridge, "join_start").mockResolvedValue(join({ state: "approved" }));
    renderLanding({ onJoined });
    await waitFor(() => expect(onJoined).toHaveBeenCalledOnce());
  });

  it("cancels a join", async () => {
    renderLanding();
    await screen.findByRole("button", { name: "Cancel" });
    fireEvent.click(screen.getByRole("button", { name: "Cancel" }));
    expect(bridge.join_cancel).toHaveBeenCalledOnce();
  });

  it("disables unavailable sign-in and identifies why", async () => {
    vi.spyOn(bridge, "hosted_account").mockResolvedValue(account({ available: false }));
    renderLanding();
    expect(await screen.findByRole("button", { name: /continue with google/i })).toBeDisabled();
    expect(screen.getByRole("alert")).toBeInTheDocument();
  });

  it.each([
    [account({ signedIn: true, classification: "lapsed" }), "hosted-lapsed-card"],
    [account({ signedIn: true, hasVault: true, access: "read_write" }), "hosted-join-panel"],
    [account({ signedIn: true, access: "read_only" }), "hosted-billing-card"],
    [account({ signedIn: true, access: "read_write", resumable: true }), "hosted-provisioning-card"],
    [account({ signedIn: true, access: "read_write" }), "hosted-passphrase-card"],
  ])("routes account states", async (value, id) => {
    vi.spyOn(bridge, "hosted_account").mockResolvedValue(value);
    renderLanding();
    await waitFor(() => expect(document.getElementById(id)).toBeInTheDocument());
  });

  it("checks and opens billing", async () => {
    vi.spyOn(bridge, "hosted_account").mockResolvedValue(account({ signedIn: true, access: "read_only" }));
    renderLanding();
    await screen.findByRole("button", { name: /check subscription/i });
    fireEvent.click(screen.getByRole("button", { name: /check subscription/i }));
    fireEvent.click(screen.getByRole("button", { name: /subscribe at peppy.pro/i }));
    expect(bridge.hosted_account).toHaveBeenCalledTimes(2);
    expect(bridge.hosted_open_billing).toHaveBeenCalledOnce();
  });

  it("provisions resumable accounts and acknowledged passphrase setup", async () => {
    vi.spyOn(bridge, "hosted_account").mockResolvedValue(account({ signedIn: true, access: "read_write", resumable: true }));
    renderLanding();
    await waitFor(() => expect(bridge.hosted_provision).toHaveBeenCalledOnce());
    cleanup();
    vi.spyOn(bridge, "hosted_account").mockResolvedValue(account({ signedIn: true, access: "read_write" }));
    renderLanding();
    const button = await screen.findByRole("button", { name: /enter passphrase securely/i });
    expect(button).toBeDisabled();
    fireEvent.click(screen.getByRole("checkbox"));
    fireEvent.click(button);
    expect(bridge.hosted_provision).toHaveBeenCalledTimes(2);
  });

  it("recovers a pending vault before offering the phone join", async () => {
    vi.spyOn(bridge, "hosted_account").mockResolvedValue(account({ signedIn: true, access: "read_only", classification: "lapsed", hasVault: true, resumable: true }));
    renderLanding();
    await waitFor(() => expect(bridge.hosted_provision).toHaveBeenCalledOnce());
    expect(document.getElementById("hosted-provisioning-card")).toBeInTheDocument();
    expect(bridge.join_start).not.toHaveBeenCalled();
  });

  it("surfaces a failed passphrase setup and refreshes the account", async () => {
    vi.spyOn(bridge, "hosted_account").mockResolvedValue(account({ signedIn: true, access: "read_write" }));
    vi.spyOn(bridge, "hosted_provision").mockRejectedValue({ code: "network", message: "offline" });
    renderLanding();
    const button = await screen.findByRole("button", { name: /enter passphrase securely/i });
    fireEvent.click(screen.getByRole("checkbox"));
    fireEvent.click(button);
    expect(await screen.findByRole("alert")).toBeInTheDocument();
    await waitFor(() => expect(bridge.hosted_account).toHaveBeenCalledTimes(2));
  });

  it("retries a hosted join at the hosted origin after visiting self-hosted", async () => {
    vi.spyOn(bridge, "join_start").mockResolvedValue(join({ state: "expired" }));
    const { rerender } = renderLanding({ mode: "self-hosted" });
    fireEvent.change(screen.getByLabelText("Server URL"), { target: { value: "https://self.example" } });
    fireEvent.click(screen.getByRole("button", { name: "Connect" }));
    await waitFor(() => expect(bridge.join_start).toHaveBeenCalledWith("https://self.example"));
    rerender(<SetupLanding mode="hosted" onMode={vi.fn()} enrolledWithoutPhone={false} pairPhone={<div />} selfHostedFallback={<div />} onJoined={vi.fn()} />);
    const retry = await screen.findByRole("button", { name: /try again/i });
    await waitFor(() => expect(retry).toBeEnabled());
    fireEvent.click(retry);
    await waitFor(() => expect(bridge.join_start).toHaveBeenLastCalledWith(null));
  });

  it("always offers the self-hosted advanced setup", () => {
    renderLanding({ mode: "self-hosted" });
    expect(document.querySelector("#self-hosted-advanced")).toHaveTextContent("Fallback setup");
  });


  it("only connects self-hosted https origins", async () => {
    const { rerender } = renderLanding({ mode: "self-hosted" });
    const input = screen.getByLabelText("Server URL");
    const connect = screen.getByRole("button", { name: "Connect" });
    fireEvent.change(input, { target: { value: "http://example.test" } });
    expect(connect).toBeDisabled();
    fireEvent.change(input, { target: { value: "https://example.test" } });
    fireEvent.click(connect);
    await waitFor(() => expect(bridge.join_start).toHaveBeenCalledWith("https://example.test"));
    rerender(<SetupLanding mode="self-hosted" onMode={vi.fn()} enrolledWithoutPhone={false} pairPhone={<div />} selfHostedFallback={<div />} onJoined={vi.fn()} />);
  });

  it("does not render a passphrase input", () => {
    renderLanding();
    expect(document.querySelector("input[type=password]")).not.toBeInTheDocument();
  });
});
