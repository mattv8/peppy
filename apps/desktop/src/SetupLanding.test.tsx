import "@testing-library/jest-dom/vitest";
import { act, cleanup, fireEvent, render, screen, waitFor } from "@testing-library/react";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { bridge, type EnrollmentView, type HostedAccountView } from "./bridge";
import { peppyCopy } from "./generated/peppyCopy";
import { SetupLanding } from "./SetupLanding";

const account = (overrides: Partial<HostedAccountView> = {}): HostedAccountView => ({
  available: true, signedIn: false, accountLabel: null, classification: null,
  entitlement: null, access: null, hasVault: false, resumable: false, ...overrides,
});
const view = (overrides: Partial<EnrollmentView> = {}): EnrollmentView => ({ mode: "community", stage: "server", ...overrides });
const waitingView = (overrides: Partial<EnrollmentView> = {}) => view({ stage: "waiting_phone", qrPayload: "secret-qr-payload", expiresInSeconds: 300, ...overrides });
const deferred = <T,>() => {
  let resolve!: (value: T) => void;
  let reject!: (reason?: unknown) => void;
  const promise = new Promise<T>((done, fail) => { resolve = done; reject = fail; });
  return { promise, resolve, reject };
};

let setStorageItem: ReturnType<typeof vi.fn>;

function renderLanding(props: Partial<React.ComponentProps<typeof SetupLanding>> = {}) {
  return render(<SetupLanding mode="hosted" onMode={vi.fn()} enrolledWithoutPhone={false} onJoined={vi.fn()} {...props} />);
}

async function chooseExisting() {
  fireEvent.click(await screen.findByRole("button", { name: /already use peppy/i }));
}

/** Captures the 4-second enrollment poll so tests can fire it deterministically. */
function capturePoll() {
  let poll: (() => void) | undefined;
  const nativeSetInterval = window.setInterval;
  vi.spyOn(window, "setInterval").mockImplementation((handler, timeout) => {
    if (timeout === 4_000) {
      poll = handler as () => void;
      return 1 as unknown as ReturnType<typeof window.setInterval>;
    }
    return nativeSetInterval(handler, timeout) as unknown as ReturnType<typeof window.setInterval>;
  });
  return async () => {
    await waitFor(() => expect(poll).toBeDefined());
    await act(async () => poll?.());
  };
}

describe("SetupLanding", () => {
  beforeEach(() => {
    const values = new Map<string, string>();
    setStorageItem = vi.fn((key: string, value: string) => values.set(key, value));
    Object.defineProperty(window, "localStorage", { configurable: true, value: { getItem: (key: string) => values.get(key) ?? null, setItem: setStorageItem, removeItem: (key: string) => values.delete(key), clear: () => values.clear() } });
    vi.restoreAllMocks();
    vi.spyOn(bridge, "hosted_account").mockResolvedValue(account());
    vi.spyOn(bridge, "hosted_provision").mockResolvedValue();
    vi.spyOn(bridge, "hosted_open_billing").mockResolvedValue();
    vi.spyOn(bridge, "hosted_sign_in").mockResolvedValue(account());
    vi.spyOn(bridge, "hosted_sign_out").mockResolvedValue();
    vi.spyOn(bridge, "enrollment_discover").mockResolvedValue(waitingView());
    vi.spyOn(bridge, "enrollment_status").mockResolvedValue(view({ stage: "ready", owner: true }));
    vi.spyOn(bridge, "enrollment_cancel").mockResolvedValue();
    vi.spyOn(bridge, "enrollment_create_owner").mockResolvedValue(view({ stage: "ready", owner: true }));
    vi.spyOn(bridge, "enrollment_sign_in").mockResolvedValue(view({ stage: "locked" }));
    vi.spyOn(bridge, "enrollment_connect_shared_secret").mockResolvedValue(view({ stage: "ready", owner: true }));
    vi.spyOn(bridge, "enrollment_add_phone").mockResolvedValue(waitingView());
    vi.spyOn(bridge, "enrollment_approve").mockResolvedValue(view({ stage: "phone_connected", owner: true }));
  });
  afterEach(() => {
    cleanup();
    vi.useRealTimers();
  });

  it("shows a chooser without starting an enrollment", async () => {
    renderLanding();
    await waitFor(() => expect(screen.getByRole("button", { name: /already use peppy/i })).toHaveFocus());
    expect(screen.getByRole("button", { name: /i'm new to peppy/i })).toBeInTheDocument();
    expect(bridge.enrollment_discover).not.toHaveBeenCalled();
  });

  it("uses a fixed self-hosted origin without hosted account calls or URL controls", async () => {
    renderLanding({ fixedOrigin: "https://community.example" });
    await waitFor(() => expect(bridge.enrollment_discover).toHaveBeenCalledWith("https://community.example"));
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
    expect(document.querySelector("#self-hosted-panel > h2")).toHaveTextContent(peppyCopy.hosted_join_headline);
    expect(link).toHaveAttribute("href", "https://account.example.com/account");
    expect(link).toHaveAttribute("target", "_blank");
    expect(link).toHaveAttribute("rel", "noopener noreferrer");
  });

  it("starts the hosted enrollment only after the existing-phone choice", async () => {
    renderLanding();
    await chooseExisting();
    await waitFor(() => expect(bridge.enrollment_discover).toHaveBeenCalled());
    expect(await screen.findByRole("img", { name: /pairing qr/i })).toBeInTheDocument();
    expect(document.body).not.toHaveTextContent("secret-qr-payload");
  });

  it("records the default Hosted choice when starting either hosted path", async () => {
    renderLanding();
    await chooseExisting();
    expect(window.localStorage.getItem("peppy.setup.mode")).toBe("hosted");

    cleanup();
    renderLanding();
    fireEvent.click(await screen.findByRole("button", { name: /i'm new to peppy/i }));
    expect(window.localStorage.getItem("peppy.setup.mode")).toBe("hosted");
  });

  it("shows only sign-in after the new-user choice", async () => {
    renderLanding();
    fireEvent.click(await screen.findByRole("button", { name: /i'm new to peppy/i }));
    expect(await screen.findByRole("button", { name: /continue with google/i })).toBeInTheDocument();
    expect(screen.queryByText(/subscribe/i)).not.toBeInTheDocument();
    expect(bridge.enrollment_discover).not.toHaveBeenCalled();
  });

  it("returns from the transient enrollment to the focused chooser and cancels the active pairing", async () => {
    renderLanding();
    await chooseExisting();
    await screen.findByRole("button", { name: "Back" });
    fireEvent.click(screen.getByRole("button", { name: "Back" }));
    expect(bridge.enrollment_cancel).toHaveBeenCalledOnce();
    await waitFor(() => expect(screen.getByRole("button", { name: /already use peppy/i })).toHaveFocus());
    fireEvent.click(screen.getByRole("button", { name: /i'm new to peppy/i }));
    fireEvent.click(await screen.findByRole("button", { name: "Back" }));
    expect(await screen.findByRole("button", { name: /already use peppy/i })).toHaveFocus();
  });

  it("does not restore a hidden enrollment when a late discovery resolves after Back", async () => {
    const discovery = deferred<EnrollmentView>();
    vi.spyOn(bridge, "enrollment_discover").mockReturnValue(discovery.promise);
    renderLanding();
    await chooseExisting();
    fireEvent.click(await screen.findByRole("button", { name: "Back" }));
    await act(async () => discovery.resolve(waitingView()));
    expect(screen.getByRole("button", { name: /already use peppy/i })).toBeInTheDocument();
    expect(document.getElementById("enrollment-panel")).not.toBeInTheDocument();
  });

  it("ignores a poll response that resolves after Back", async () => {
    const advance = capturePoll();
    const status = deferred<EnrollmentView>();
    vi.spyOn(bridge, "enrollment_status").mockReturnValue(status.promise);
    renderLanding();
    await chooseExisting();
    await screen.findByRole("img", { name: /pairing qr/i });
    await advance();
    fireEvent.click(screen.getByRole("button", { name: "Back" }));
    await act(async () => status.resolve(view({ stage: "verify_phone", verificationCode: "123456" })));
    expect(document.getElementById("enrollment-panel")).not.toBeInTheDocument();
    expect(screen.getByRole("button", { name: /already use peppy/i })).toBeInTheDocument();
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

  it("keeps signed-in routing precedence and enrolls only an existing vault", async () => {
    vi.spyOn(bridge, "hosted_account").mockResolvedValue(account({ signedIn: true, classification: "lapsed", hasVault: true, resumable: false }));
    renderLanding();
    await waitFor(() => expect(document.getElementById("hosted-lapsed-card")).toBeInTheDocument());
    expect(bridge.enrollment_discover).not.toHaveBeenCalled();
    cleanup();
    vi.spyOn(bridge, "hosted_account").mockResolvedValue(account({ signedIn: true, hasVault: true, access: "read_write" }));
    renderLanding();
    await waitFor(() => expect(bridge.enrollment_discover).toHaveBeenCalledOnce());
    expect(document.getElementById("hosted-path-chooser")).not.toBeInTheDocument();
  });

  it("routes a newly signed-in existing-vault account into hosted enrollment", async () => {
    vi.spyOn(bridge, "hosted_sign_in").mockResolvedValue(account({ signedIn: true, hasVault: true, access: "read_write" }));
    renderLanding();
    fireEvent.click(await screen.findByRole("button", { name: /i'm new to peppy/i }));
    fireEvent.click(await screen.findByRole("button", { name: /continue with google/i }));
    await waitFor(() => expect(bridge.enrollment_discover).toHaveBeenCalled());
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
    expect(bridge.enrollment_discover).not.toHaveBeenCalled();
  });

  it("resets the transient hosted path when controlled mode changes", async () => {
    const view1 = renderLanding();
    await chooseExisting();
    view1.rerender(<SetupLanding mode="self-hosted" onMode={vi.fn()} enrolledWithoutPhone={false} onJoined={vi.fn()} />);
    expect(screen.getByLabelText("Server URL")).toBeInTheDocument();
    view1.rerender(<SetupLanding mode="hosted" onMode={vi.fn()} enrolledWithoutPhone={false} onJoined={vi.fn()} />);
    expect(await screen.findByRole("button", { name: /already use peppy/i })).toBeInTheDocument();
    expect(bridge.enrollment_cancel).toHaveBeenCalled();
  });

  it("keeps native passphrase entry and hosted provisioning routing", async () => {
    vi.spyOn(bridge, "hosted_account").mockResolvedValue(account({ signedIn: true, access: "read_write" }));
    renderLanding();
    expect(await screen.findByRole("button", { name: /enter passphrase securely/i })).toBeDisabled();
    expect(document.querySelector("input[type=password]")).not.toBeInTheDocument();
  });

  it("offers the enrolled-without-phone owner the connect-phone choice from the enrollment stages", async () => {
    renderLanding({ enrolledWithoutPhone: true });
    expect(await screen.findByRole("heading", { name: /connect your phone/i })).toBeInTheDocument();
    expect(screen.getByRole("radio", { name: /transfer keys securely/i })).toBeChecked();
    expect(screen.getByRole("button", { name: /add phone/i })).toBeInTheDocument();
    expect(screen.queryByLabelText("Server mode")).not.toBeInTheDocument();
    expect(bridge.enrollment_status).toHaveBeenCalled();
    expect(bridge.hosted_account).not.toHaveBeenCalled();
  });

  it("keeps billing, provisioning, and passphrase errors on their established routes", async () => {
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

  it("recovers a resumable vault before starting a phone enrollment", async () => {
    vi.spyOn(bridge, "hosted_account").mockResolvedValue(account({ signedIn: true, access: "read_only", classification: "lapsed", hasVault: true, resumable: true }));
    renderLanding();
    await waitFor(() => expect(bridge.hosted_provision).toHaveBeenCalledOnce());
    expect(bridge.enrollment_discover).not.toHaveBeenCalled();
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

  it("records Hosted when an explicit passphrase provision begins", async () => {
    vi.spyOn(bridge, "hosted_account").mockResolvedValue(account({ signedIn: true, access: "read_write" }));
    renderLanding();
    fireEvent.click(await screen.findByRole("checkbox"));
    fireEvent.click(screen.getByRole("button", { name: /enter passphrase securely/i }));
    expect(window.localStorage.getItem("peppy.setup.mode")).toBe("hosted");
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

  it("discovers the entered self-hosted origin before offering hosted choices", async () => {
    const view1 = renderLanding({ mode: "self-hosted" });
    fireEvent.change(screen.getByLabelText("Server URL"), { target: { value: "https://self.example" } });
    fireEvent.click(screen.getByRole("button", { name: "Connect" }));
    await waitFor(() => expect(bridge.enrollment_discover).toHaveBeenCalledWith("https://self.example"));
    view1.rerender(<SetupLanding mode="hosted" onMode={vi.fn()} enrolledWithoutPhone={false} onJoined={vi.fn()} />);
    fireEvent.click(await screen.findByRole("button", { name: /already use peppy/i }));
    await waitFor(() => expect(bridge.enrollment_discover).toHaveBeenCalledTimes(2));
  });

  it("only connects self-hosted HTTPS origins and has no legacy advanced fallback", async () => {
    renderLanding({ mode: "self-hosted" });
    const input = screen.getByLabelText("Server URL");
    const connect = screen.getByRole("button", { name: "Connect" });
    fireEvent.change(input, { target: { value: "http://example.test" } });
    expect(connect).toBeDisabled();
    fireEvent.change(input, { target: { value: "https://example.test" } });
    fireEvent.click(connect);
    await waitFor(() => expect(bridge.enrollment_discover).toHaveBeenCalledWith("https://example.test"));
    expect(document.getElementById("self-hosted-advanced")).not.toBeInTheDocument();
    expect(screen.queryByText(/credential/i)).not.toBeInTheDocument();
  });

  it("records Self-hosted only when an explicit connection begins", async () => {
    renderLanding({ mode: "self-hosted" });
    expect(setStorageItem).not.toHaveBeenCalled();
    fireEvent.change(screen.getByLabelText("Server URL"), { target: { value: "https://example.test" } });
    fireEvent.click(screen.getByRole("button", { name: "Connect" }));
    expect(window.localStorage.getItem("peppy.setup.mode")).toBe("self-hosted");
  });

  it("does not record a choice for automatic resumable provisioning or fixed-origin enrollment", async () => {
    vi.spyOn(bridge, "hosted_account").mockResolvedValue(account({ signedIn: true, access: "read_write", resumable: true }));
    renderLanding();
    await waitFor(() => expect(bridge.hosted_provision).toHaveBeenCalled());
    expect(setStorageItem).not.toHaveBeenCalled();

    cleanup();
    renderLanding({ fixedOrigin: "https://community.example" });
    await waitFor(() => expect(bridge.enrollment_discover).toHaveBeenCalledWith("https://community.example"));
    expect(setStorageItem).not.toHaveBeenCalled();
  });

  it("persists the selected server mode and notifies the parent", () => {
    const onMode = vi.fn();
    renderLanding({ onMode });
    fireEvent.change(screen.getByRole("combobox", { name: "Server mode" }), { target: { value: "self-hosted" } });
    expect(onMode).toHaveBeenCalledWith("self-hosted");
    expect(window.localStorage.getItem("peppy.setup.mode")).toBe("self-hosted");
  });

  it("does not reset an active enrollment when Hosted is selected again", async () => {
    renderLanding();
    await chooseExisting();
    await waitFor(() => expect(bridge.enrollment_discover).toHaveBeenCalled());
    fireEvent.change(screen.getByRole("combobox", { name: "Server mode" }), { target: { value: "hosted" } });
    expect(bridge.enrollment_cancel).not.toHaveBeenCalled();
  });

  it("keeps the setup heading while routing enrolled desktops to phone enrollment", async () => {
    renderLanding({ enrolledWithoutPhone: true });
    expect(screen.getByRole("heading", { level: 1 })).toHaveTextContent("Set up Peppy");
    expect(screen.queryByLabelText("Server mode")).not.toBeInTheDocument();
    await screen.findByRole("heading", { name: /connect your phone/i });
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
    const view1 = renderLanding({ mode: "self-hosted" });
    view1.rerender(<SetupLanding mode="hosted" onMode={vi.fn()} enrolledWithoutPhone={false} onJoined={vi.fn()} />);
    await waitFor(() => expect(bridge.hosted_account).toHaveBeenCalled());
    await act(async () => request.resolve(fresh));
    await waitFor(() => expect(bridge.enrollment_discover).toHaveBeenCalled());
  });

  it("cancels an active self-hosted enrollment when unmounted", async () => {
    const view1 = renderLanding({ mode: "self-hosted" });
    fireEvent.change(screen.getByLabelText("Server URL"), { target: { value: "https://self.example" } });
    fireEvent.click(screen.getByRole("button", { name: "Connect" }));
    await screen.findByRole("img", { name: /pairing qr/i });
    view1.unmount();
    expect(bridge.enrollment_cancel).toHaveBeenCalled();
  });

  it("does not route on account check during billing refresh", async () => {
    const request = deferred<HostedAccountView>();
    vi.spyOn(bridge, "hosted_account").mockReturnValue(request.promise);
    renderLanding();
    await act(async () => request.resolve(account({ signedIn: true, access: "read_only" })));
    await screen.findByRole("button", { name: /check subscription/i });
    expect(document.getElementById("hosted-billing-card")).toBeInTheDocument();
    const refreshRequest = deferred<HostedAccountView>();
    vi.spyOn(bridge, "hosted_account").mockReturnValueOnce(refreshRequest.promise);
    act(() => { window.dispatchEvent(new Event("focus")); });
    expect(document.getElementById("hosted-billing-card")).toBeInTheDocument();
    await act(async () => refreshRequest.resolve(account({ signedIn: true, access: "read_only" })));
    expect(document.getElementById("hosted-billing-card")).toBeInTheDocument();
  });

  describe("sanitized enrollment stages", () => {
    it("sends the entered server origin to discovery and renders no secret fields", async () => {
      renderLanding({ mode: "self-hosted" });
      fireEvent.change(screen.getByLabelText("Server URL"), { target: { value: "https://server.example" } });
      fireEvent.click(screen.getByRole("button", { name: "Connect" }));
      await waitFor(() => expect(bridge.enrollment_discover).toHaveBeenCalledWith("https://server.example"));
      expect(document.querySelector("input[type=password]")).not.toBeInTheDocument();
    });

    it("creates the first owner through the native prompt and never shows a password field", async () => {
      vi.spyOn(bridge, "enrollment_discover").mockResolvedValue(view({ stage: "setup" }));
      renderLanding({ mode: "self-hosted" });
      fireEvent.change(screen.getByLabelText("Server URL"), { target: { value: "https://server.example" } });
      fireEvent.click(screen.getByRole("button", { name: "Connect" }));
      fireEvent.click(await screen.findByRole("button", { name: "Create server" }));
      await waitFor(() => expect(bridge.enrollment_create_owner).toHaveBeenCalledOnce());
      expect(document.querySelector("input[type=password]")).not.toBeInTheDocument();
    });

    it("explains open setup without claiming that a setup-code prompt will appear", async () => {
      vi.spyOn(bridge, "enrollment_discover").mockResolvedValue(view({ stage: "setup", setupCodeRequired: false }));
      renderLanding({ mode: "self-hosted" });
      fireEvent.change(screen.getByLabelText("Server URL"), { target: { value: "https://server.example" } });
      fireEvent.click(screen.getByRole("button", { name: "Connect" }));

      await screen.findByRole("heading", { name: "Set up your server" });
      const setup = document.getElementById("enrollment-setup")!;
      expect(setup).toHaveAttribute("data-setup-mode", "open");
      expect(setup).toHaveTextContent(peppyCopy.enrollment_setup_open);
      expect(setup).not.toHaveTextContent("setup code");
    });

    it("uses protected setup copy when discovery omits the policy", async () => {
      vi.spyOn(bridge, "enrollment_discover").mockResolvedValue(view({ stage: "setup" }));
      renderLanding({ mode: "self-hosted" });
      fireEvent.change(screen.getByLabelText("Server URL"), { target: { value: "https://server.example" } });
      fireEvent.click(screen.getByRole("button", { name: "Connect" }));

      await screen.findByRole("heading", { name: "Set up your server" });
      const setup = document.getElementById("enrollment-setup")!;
      expect(setup).toHaveAttribute("data-setup-mode", "protected");
      expect(setup).toHaveTextContent(peppyCopy.enrollment_setup_protected);
    });

    it("shows sanitized claim feedback for known native error codes", async () => {
      vi.spyOn(bridge, "enrollment_discover").mockResolvedValue(view({ stage: "setup", setupCodeRequired: true }));
      vi.spyOn(bridge, "enrollment_create_owner")
        .mockResolvedValueOnce(view({ stage: "error", errorCode: "invalid_setup_token" }))
        .mockResolvedValueOnce(view({ stage: "ready", owner: true }));
      renderLanding({ mode: "self-hosted" });
      fireEvent.change(screen.getByLabelText("Server URL"), { target: { value: "https://server.example" } });
      fireEvent.click(screen.getByRole("button", { name: "Connect" }));
      fireEvent.click(await screen.findByRole("button", { name: "Create server" }));

      expect(await screen.findByRole("alert")).toHaveTextContent(peppyCopy.enrollment_error_invalid_setup_token);
      fireEvent.click(screen.getByRole("button", { name: "Retry" }));
      await waitFor(() => expect(bridge.enrollment_create_owner).toHaveBeenCalledTimes(2));
    });

    it("cancels the setup stage back to the server address without a bridge call", async () => {
      vi.spyOn(bridge, "enrollment_discover").mockResolvedValue(view({ stage: "setup" }));
      renderLanding({ mode: "self-hosted" });
      fireEvent.change(screen.getByLabelText("Server URL"), { target: { value: "https://server.example" } });
      fireEvent.click(screen.getByRole("button", { name: "Connect" }));
      fireEvent.click(await screen.findByRole("button", { name: "Cancel" }));
      expect(await screen.findByLabelText("Server URL")).toHaveValue("https://server.example");
      expect(bridge.enrollment_create_owner).not.toHaveBeenCalled();
    });

    it("signs in to a self-hosted server through the native prompt", async () => {
      vi.spyOn(bridge, "enrollment_discover").mockResolvedValue(view({ stage: "signin" }));
      renderLanding({ mode: "self-hosted" });
      fireEvent.change(screen.getByLabelText("Server URL"), { target: { value: "https://server.example" } });
      fireEvent.click(screen.getByRole("button", { name: "Connect" }));
      fireEvent.click(await screen.findByRole("button", { name: "Sign in" }));
      await waitFor(() => expect(bridge.enrollment_sign_in).toHaveBeenCalledOnce());
      expect(document.body).not.toHaveTextContent(/admin password/i);
    });

    it("signs in to a hosted stage through the enrollment bridge exactly once", async () => {
      vi.spyOn(bridge, "enrollment_discover").mockResolvedValue(view({ stage: "signin", mode: "hosted" }));
      vi.spyOn(bridge, "enrollment_sign_in").mockResolvedValue(view({ stage: "locked", mode: "hosted" }));
      renderLanding();
      fireEvent.click(await screen.findByRole("button", { name: /already use peppy/i }));
      fireEvent.click(await screen.findByRole("button", { name: "Sign in" }));
      expect(await screen.findByRole("button", { name: "Enter shared secret" })).toBeInTheDocument();
      expect(bridge.enrollment_sign_in).toHaveBeenCalledOnce();
      expect(bridge.hosted_sign_in).not.toHaveBeenCalled();
      expect(bridge.enrollment_discover).toHaveBeenCalledTimes(1);
    });

    it("uses the community sign-in bridge for a fixed origin even when the outer mode is hosted", async () => {
      vi.spyOn(bridge, "enrollment_discover").mockResolvedValue(view({ stage: "signin", mode: "community", origin: "https://community.example" }));
      renderLanding({ fixedOrigin: "https://community.example" });
      expect(await screen.findByRole("heading", { name: "Sign in to your server" })).toBeInTheDocument();
      expect(document.body).not.toHaveTextContent(/peppy account|hosted/i);
      fireEvent.click(screen.getByRole("button", { name: "Sign in" }));
      await waitFor(() => expect(bridge.enrollment_sign_in).toHaveBeenCalledOnce());
      expect(bridge.hosted_sign_in).not.toHaveBeenCalled();
    });

    it("lets a community user connect with the shared secret without an admin sign-in", async () => {
      vi.spyOn(bridge, "enrollment_discover").mockResolvedValue(view({ stage: "signin", mode: "community", origin: "https://community.example" }));
      renderLanding({ fixedOrigin: "https://community.example" });
      expect(await screen.findByText("The admin password manages this server. The vault shared secret connects this device to encrypted data.")).toBeInTheDocument();
      fireEvent.click(screen.getByRole("button", { name: "Connect with shared secret" }));
      await waitFor(() => expect(bridge.enrollment_connect_shared_secret).toHaveBeenCalledOnce());
      expect(bridge.enrollment_sign_in).not.toHaveBeenCalled();
      expect(document.querySelector("input[type=password]")).not.toBeInTheDocument();
    });

    it("keeps the shared-secret path off the hosted sign-in stage", async () => {
      vi.spyOn(bridge, "enrollment_discover").mockResolvedValue(view({ stage: "signin", mode: "hosted" }));
      renderLanding();
      fireEvent.click(await screen.findByRole("button", { name: /already use peppy/i }));
      expect(await screen.findByRole("button", { name: "Sign in" })).toBeInTheDocument();
      expect(screen.queryByRole("button", { name: "Connect with shared secret" })).not.toBeInTheDocument();
      expect(screen.queryByText(/The admin password manages this server/)).not.toBeInTheDocument();
    });

    it("keeps the entered server address when the shared secret is wrong", async () => {
      vi.spyOn(bridge, "enrollment_connect_shared_secret").mockResolvedValue(view({ stage: "error", origin: "https://server.example", errorCode: "invalid-request" }));
      vi.spyOn(bridge, "enrollment_discover").mockResolvedValue(view({ stage: "signin", mode: "community", origin: "https://server.example" }));
      renderLanding({ mode: "self-hosted" });
      fireEvent.change(screen.getByLabelText("Server URL"), { target: { value: "https://server.example" } });
      fireEvent.click(screen.getByRole("button", { name: "Connect" }));
      fireEvent.click(await screen.findByRole("button", { name: "Connect with shared secret" }));
      expect(await screen.findByRole("alert")).toHaveTextContent("Your server address was kept");
      expect(screen.getByLabelText("Server URL")).toHaveValue("https://server.example");
    });

    it("sets up a first hosted vault without an admin password prompt", async () => {
      vi.spyOn(bridge, "enrollment_discover").mockResolvedValue(view({ stage: "setup", mode: "hosted" }));
      renderLanding();
      fireEvent.click(await screen.findByRole("button", { name: /already use peppy/i }));
      expect(await screen.findByRole("heading", { name: "Create your vault" })).toBeInTheDocument();
      expect(document.body).not.toHaveTextContent(/admin password|setup code/i);
      expect(screen.queryByRole("button", { name: "Create server" })).not.toBeInTheDocument();
      fireEvent.click(screen.getByRole("button", { name: "Create vault" }));
      await waitFor(() => expect(bridge.enrollment_create_owner).toHaveBeenCalledOnce());
    });

    it("enters the shared secret through the bridge instead of a React input", async () => {
      vi.spyOn(bridge, "enrollment_discover").mockResolvedValue(view({ stage: "locked" }));
      renderLanding({ mode: "self-hosted" });
      fireEvent.change(screen.getByLabelText("Server URL"), { target: { value: "https://server.example" } });
      fireEvent.click(screen.getByRole("button", { name: "Connect" }));
      fireEvent.click(await screen.findByRole("button", { name: "Enter shared secret" }));
      await waitFor(() => expect(bridge.enrollment_connect_shared_secret).toHaveBeenCalledOnce());
      expect(document.querySelector("input[type=password]")).not.toBeInTheDocument();
    });

    it("lets an owner choose a phone method and forwards it to add-phone", async () => {
      vi.spyOn(bridge, "enrollment_discover").mockResolvedValue(view({ stage: "ready", owner: true }));
      renderLanding({ mode: "self-hosted" });
      fireEvent.change(screen.getByLabelText("Server URL"), { target: { value: "https://server.example" } });
      fireEvent.click(screen.getByRole("button", { name: "Connect" }));
      fireEvent.click(await screen.findByRole("radio", { name: /enter shared secret on phone/i }));
      fireEvent.click(screen.getByRole("button", { name: "Add phone" }));
      await waitFor(() => expect(bridge.enrollment_add_phone).toHaveBeenCalledWith("shared_secret"));
    });

    it("shows a waiting message instead of an owner action to a non-owner device", async () => {
      vi.spyOn(bridge, "enrollment_status").mockResolvedValue(view({ stage: "ready", owner: false }));
      renderLanding({ enrolledWithoutPhone: true });
      expect(await screen.findByRole("heading", { name: /waiting for an owner/i })).toBeInTheDocument();
      expect(screen.queryByRole("button", { name: "Add phone" })).not.toBeInTheDocument();
    });

    it("renders exactly one QR from the enrollment payload and offers no save action", async () => {
      renderLanding({ mode: "self-hosted" });
      fireEvent.change(screen.getByLabelText("Server URL"), { target: { value: "https://server.example" } });
      fireEvent.click(screen.getByRole("button", { name: "Connect" }));
      await screen.findByRole("img", { name: /pairing qr/i });
      expect(document.querySelectorAll("#enrollment-qr-image")).toHaveLength(1);
      expect(screen.queryByRole("button", { name: /save/i })).not.toBeInTheDocument();
      expect(screen.getByText(/does not grant any permissions/i)).toBeInTheDocument();
      expect(document.body).not.toHaveTextContent("secret-qr-payload");
    });

    it("cancels a waiting pairing and refreshes the enrollment stage", async () => {
      renderLanding({ mode: "self-hosted" });
      fireEvent.change(screen.getByLabelText("Server URL"), { target: { value: "https://server.example" } });
      fireEvent.click(screen.getByRole("button", { name: "Connect" }));
      await screen.findByRole("img", { name: /pairing qr/i });
      fireEvent.click(screen.getByRole("button", { name: "Cancel" }));
      await waitFor(() => expect(bridge.enrollment_cancel).toHaveBeenCalledOnce());
      await waitFor(() => expect(bridge.enrollment_status).toHaveBeenCalled());
    });

    it("moves from a polled waiting view to the verification stage without showing the QR", async () => {
      const advance = capturePoll();
      vi.spyOn(bridge, "enrollment_status").mockResolvedValue(view({ stage: "verify_phone", verificationCode: "382941" }));
      renderLanding({ mode: "self-hosted" });
      fireEvent.change(screen.getByLabelText("Server URL"), { target: { value: "https://server.example" } });
      fireEvent.click(screen.getByRole("button", { name: "Connect" }));
      await screen.findByRole("img", { name: /pairing qr/i });
      await advance();
      expect(await screen.findByLabelText("Verification code: 382941")).toBeInTheDocument();
      expect(document.getElementById("enrollment-qr-image")).not.toBeInTheDocument();
    });

    it("keeps approval disabled until the codes-match checkbox is checked", async () => {
      vi.spyOn(bridge, "enrollment_discover").mockResolvedValue(view({ stage: "verify_phone", verificationCode: "382941" }));
      renderLanding({ mode: "self-hosted" });
      fireEvent.change(screen.getByLabelText("Server URL"), { target: { value: "https://server.example" } });
      fireEvent.click(screen.getByRole("button", { name: "Connect" }));
      const approve = await screen.findByRole("button", { name: "Confirm and approve" });
      expect(approve).toBeDisabled();
      fireEvent.click(screen.getByRole("checkbox", { name: /codes match/i }));
      expect(approve).toBeEnabled();
      fireEvent.click(approve);
      await waitFor(() => expect(bridge.enrollment_approve).toHaveBeenCalledOnce());
    });

    it("reports a connected phone to the parent exactly once", async () => {
      const onJoined = vi.fn();
      vi.spyOn(bridge, "enrollment_discover").mockResolvedValue(view({ stage: "phone_connected", owner: true }));
      renderLanding({ mode: "self-hosted", onJoined });
      fireEvent.change(screen.getByLabelText("Server URL"), { target: { value: "https://server.example" } });
      fireEvent.click(screen.getByRole("button", { name: "Connect" }));
      await waitFor(() => expect(onJoined).toHaveBeenCalledOnce());
      expect(document.getElementById("enrollment-phone_connected")).toHaveTextContent(/finish setup on your phone/i);
    });

    it("restarts an expired pairing with the previously chosen method", async () => {
      vi.spyOn(bridge, "enrollment_discover").mockResolvedValue(view({ stage: "ready", owner: true }));
      vi.spyOn(bridge, "enrollment_add_phone").mockResolvedValueOnce(view({ stage: "expired" })).mockResolvedValueOnce(waitingView());
      renderLanding({ mode: "self-hosted" });
      fireEvent.change(screen.getByLabelText("Server URL"), { target: { value: "https://server.example" } });
      fireEvent.click(screen.getByRole("button", { name: "Connect" }));
      fireEvent.click(await screen.findByRole("radio", { name: /enter shared secret on phone/i }));
      fireEvent.click(screen.getByRole("button", { name: "Add phone" }));
      fireEvent.click(await screen.findByRole("button", { name: "Start again" }));
      await waitFor(() => expect(bridge.enrollment_add_phone).toHaveBeenCalledTimes(2));
      expect(bridge.enrollment_add_phone).toHaveBeenLastCalledWith("shared_secret");
      expect(await screen.findByRole("img", { name: /pairing qr/i })).toBeInTheDocument();
    });

    it("shows a generic error, keeps the server address, and retries the failed step", async () => {
      vi.spyOn(bridge, "enrollment_discover").mockRejectedValueOnce(new Error("raw remote text")).mockResolvedValueOnce(waitingView());
      renderLanding({ mode: "self-hosted" });
      fireEvent.change(screen.getByLabelText("Server URL"), { target: { value: "https://server.example" } });
      fireEvent.click(screen.getByRole("button", { name: "Connect" }));
      expect(await screen.findByRole("alert")).toHaveTextContent("Peppy could not complete this step");
      expect(document.body).not.toHaveTextContent("raw remote text");
      expect(screen.getByLabelText("Server URL")).toHaveValue("https://server.example");
      fireEvent.click(screen.getByRole("button", { name: "Retry" }));
      expect(await screen.findByRole("img", { name: /pairing qr/i })).toBeInTheDocument();
      expect(bridge.enrollment_discover).toHaveBeenCalledTimes(2);
    });

    it("ignores a poll that was in flight when the user cancelled", async () => {
      const advance = capturePoll();
      const stalePoll = deferred<EnrollmentView>();
      vi.spyOn(bridge, "enrollment_status").mockReturnValueOnce(stalePoll.promise).mockResolvedValueOnce(view({ stage: "ready", owner: true }));
      renderLanding({ mode: "self-hosted" });
      fireEvent.change(screen.getByLabelText("Server URL"), { target: { value: "https://server.example" } });
      fireEvent.click(screen.getByRole("button", { name: "Connect" }));
      await screen.findByRole("img", { name: /pairing qr/i });
      await advance();
      fireEvent.click(screen.getByRole("button", { name: "Cancel" }));
      await waitFor(() => expect(screen.getByRole("button", { name: "Add phone" })).toBeInTheDocument());
      await act(async () => stalePoll.resolve(waitingView()));
      expect(document.getElementById("enrollment-qr-image")).not.toBeInTheDocument();
      expect(screen.getByRole("button", { name: "Add phone" })).toBeInTheDocument();
    });
  });

  describe("self-hosted server URL validation", () => {
    function renderSelfHosted() {
      renderLanding({ mode: "self-hosted" });
      return { input: screen.getByLabelText("Server URL"), connect: screen.getByRole("button", { name: "Connect" }) };
    }

    it.each(["http://127.0.0.1:7000", "http://localhost:3000", "http://[::1]:8080", "http://127.8.9.10:7000"])("enables Connect for explicit loopback HTTP %s", value => {
      const { input, connect } = renderSelfHosted();
      fireEvent.change(input, { target: { value } });
      expect(connect).toBeEnabled();
      expect(document.getElementById("self-hosted-url-error")).not.toBeInTheDocument();
    });

    it("connects loopback HTTP with its origin", async () => {
      const { input, connect } = renderSelfHosted();
      fireEvent.change(input, { target: { value: "http://127.0.0.1:7000" } });
      fireEvent.click(connect);
      await waitFor(() => expect(bridge.enrollment_discover).toHaveBeenCalledWith("http://127.0.0.1:7000"));
    });

    it("keeps a root slash on an otherwise valid HTTPS origin", () => {
      const { input, connect } = renderSelfHosted();
      fireEvent.change(input, { target: { value: "https://server.example/" } });
      expect(connect).toBeEnabled();
    });

    it.each(["https://server.example/?", "https://server.example/#", "HTTPS://SERVER.EXAMPLE/?", "http://127.0.0.1:7000/?", "http://localhost:3000/#"])("rejects a bare query or fragment delimiter in %s", value => {
      const { input, connect } = renderSelfHosted();
      fireEvent.change(input, { target: { value } });
      fireEvent.blur(input);
      expect(connect).toBeDisabled();
      expect(document.getElementById("self-hosted-url-error")).toHaveTextContent(peppyCopy.setup_self_hosted_url_path);
    });

    it("normalizes the uppercase loopback origin before connecting", async () => {
      const { input, connect } = renderSelfHosted();
      fireEvent.change(input, { target: { value: "HTTP://LOCALHOST:3000" } });
      expect(connect).toBeEnabled();
      fireEvent.click(connect);
      await waitFor(() => expect(bridge.enrollment_discover).toHaveBeenCalledWith("http://localhost:3000"));
    });

    it("does not show an error while the field is still being typed in", () => {
      const { input } = renderSelfHosted();
      fireEvent.change(input, { target: { value: "http://example.test" } });
      expect(document.getElementById("self-hosted-url-error")).not.toBeInTheDocument();
    });

    it("does not show an error for an empty field after blur", () => {
      const { input } = renderSelfHosted();
      fireEvent.blur(input);
      expect(document.getElementById("self-hosted-url-error")).not.toBeInTheDocument();
      expect(input).not.toHaveAttribute("aria-invalid", "true");
    });

    it.each([
      ["non-loopback HTTP", "http://example.test", "setup_self_hosted_url_https_required"],
      ["lookalike loopback host", "http://localhost.evil.test:8080", "setup_self_hosted_url_https_required"],
      ["lookalike 127.0.0.1 host", "http://127.0.0.1.evil.test:7000", "setup_self_hosted_url_https_required"],
      ["credentials", "https://user:pass@server.example", "setup_self_hosted_url_credentials"],
      ["loopback HTTP with credentials", "http://user@127.0.0.1:7000", "setup_self_hosted_url_credentials"],
      ["HTTPS path", "https://server.example/api/v1", "setup_self_hosted_url_path"],
      ["loopback HTTP path", "http://127.0.0.1:7000/path", "setup_self_hosted_url_path"],
      ["query", "https://server.example/?x=1", "setup_self_hosted_url_path"],
      ["fragment", "http://localhost:3000/#top", "setup_self_hosted_url_path"],
      ["unsupported scheme", "ftp://server.example", "setup_self_hosted_url_invalid"],
      ["unparseable text", "not a url", "setup_self_hosted_url_invalid"],
    ] as const)("rejects %s with a visible error after blur", (_label, value, errorKey) => {
      const { input, connect } = renderSelfHosted();
      fireEvent.change(input, { target: { value } });
      fireEvent.blur(input);
      expect(connect).toBeDisabled();
      expect(input).toHaveAttribute("aria-invalid", "true");
      const error = document.getElementById("self-hosted-url-error");
      expect(error).toHaveTextContent(peppyCopy[errorKey]);
      expect(error).toHaveAttribute("aria-live", "polite");
      expect(input).toHaveAttribute("aria-describedby", "self-hosted-url-error self-hosted-url-hint");
      expect(input).toHaveAccessibleDescription(expect.stringContaining(peppyCopy[errorKey]));
    });

    it("keeps the error live and clears it once the value is corrected", () => {
      const { input, connect } = renderSelfHosted();
      fireEvent.change(input, { target: { value: "https://server.example/api" } });
      fireEvent.blur(input);
      expect(document.getElementById("self-hosted-url-error")).toHaveTextContent(peppyCopy.setup_self_hosted_url_path);
      fireEvent.change(input, { target: { value: "https://server.example" } });
      expect(document.getElementById("self-hosted-url-error")).not.toBeInTheDocument();
      expect(input).not.toHaveAttribute("aria-invalid", "true");
      expect(connect).toBeEnabled();
    });
  });
});
