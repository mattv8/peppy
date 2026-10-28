import "@testing-library/jest-dom/vitest";
import { act, cleanup, fireEvent, render, screen, waitFor, within } from "@testing-library/react";
import QRCode from "qrcode";
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
    Object.assign(bridge, { hosted_origin_url: vi.fn().mockResolvedValue("https://peppy.pro") });
    vi.spyOn(bridge, "enrollment_discover").mockResolvedValue(waitingView());
    vi.spyOn(bridge, "enrollment_start_over").mockResolvedValue(view({ stage: "setup" }));
    vi.spyOn(bridge, "enrollment_status").mockResolvedValue(view({ stage: "ready", owner: true, hasGateway: false }));
    vi.spyOn(bridge, "enrollment_cancel").mockResolvedValue();
    vi.spyOn(bridge, "enrollment_create_owner").mockResolvedValue(view({ stage: "ready", owner: true, hasGateway: false }));
    vi.spyOn(bridge, "enrollment_sign_in").mockResolvedValue(view({ stage: "locked" }));
    vi.spyOn(bridge, "enrollment_connect_shared_secret").mockResolvedValue(view({ stage: "ready", owner: true, hasGateway: false }));
    vi.spyOn(bridge, "enrollment_add_phone").mockResolvedValue(waitingView());
    vi.spyOn(bridge, "enrollment_rotate_phone").mockResolvedValue(waitingView());
    vi.spyOn(bridge, "enrollment_cancel_phone").mockResolvedValue(view({ stage: "ready", owner: true, hasGateway: false }));
    vi.spyOn(bridge, "reveal_vault_secret").mockResolvedValue();
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

  it("offers start over only for a resumed owner setup and invokes it after confirmation", async () => {
    vi.spyOn(bridge, "enrollment_discover").mockResolvedValue(view({ stage: "setup", ownerResumePending: true }));
    renderLanding({ fixedOrigin: "https://community.example" });

    fireEvent.click(await screen.findByRole("button", { name: "Start over" }));
    const confirm = screen.getByRole("button", { name: "Discard and start over" });
    expect(confirm).toHaveFocus();
    fireEvent.click(confirm);
    await waitFor(() => expect(bridge.enrollment_start_over).toHaveBeenCalledOnce());
    expect(document.getElementById("enrollment-resume-notice")).not.toBeInTheDocument();
    await waitFor(() => expect(screen.getByRole("button", { name: "Create server" })).toHaveFocus());
  });

  it("cancels a resumed owner start-over confirmation without calling the bridge", async () => {
    vi.spyOn(bridge, "enrollment_discover").mockResolvedValue(view({ stage: "setup", ownerResumePending: true }));
    renderLanding({ fixedOrigin: "https://community.example" });

    fireEvent.click(await screen.findByRole("button", { name: "Start over" }));
    fireEvent.click(screen.getByRole("button", { name: "Keep my earlier attempt" }));
    expect(bridge.enrollment_start_over).not.toHaveBeenCalled();
    await waitFor(() => expect(screen.getByRole("button", { name: "Start over" })).toHaveFocus());
  });

  it("shows the typed start-over failure", async () => {
    vi.spyOn(bridge, "enrollment_discover").mockResolvedValue(view({ stage: "setup", ownerResumePending: true }));
    vi.spyOn(bridge, "enrollment_start_over").mockRejectedValue({ code: "instance_already_claimed" });
    renderLanding({ fixedOrigin: "https://community.example" });

    fireEvent.click(await screen.findByRole("button", { name: "Start over" }));
    fireEvent.click(screen.getByRole("button", { name: "Discard and start over" }));
    expect(await screen.findByRole("alert")).toBeInTheDocument();
    expect(document.getElementById("enrollment-error-code")).toHaveAttribute("data-error-code", "instance_already_claimed");
  });

  it("retries start-over verification with discovery instead of repeating the discard", async () => {
    vi.spyOn(bridge, "enrollment_discover").mockResolvedValue(view({ stage: "setup", ownerResumePending: true, origin: "https://community.example" }));
    vi.spyOn(bridge, "enrollment_start_over").mockRejectedValue({ code: "offline" });
    renderLanding({ fixedOrigin: "https://community.example" });

    fireEvent.click(await screen.findByRole("button", { name: "Start over" }));
    fireEvent.click(screen.getByRole("button", { name: "Discard and start over" }));
    await screen.findByRole("alert");
    fireEvent.click(screen.getByRole("button", { name: "Retry" }));

    await waitFor(() => expect(bridge.enrollment_discover).toHaveBeenCalledTimes(2));
    expect(bridge.enrollment_start_over).toHaveBeenCalledOnce();
  });

  it("does not reuse start-over wording for a later create-owner claim conflict", async () => {
    const resumed = view({ stage: "setup", ownerResumePending: true, origin: "https://community.example" });
    vi.spyOn(bridge, "enrollment_discover").mockResolvedValueOnce(resumed).mockResolvedValueOnce(resumed);
    vi.spyOn(bridge, "enrollment_start_over").mockRejectedValue({ code: "offline" });
    vi.spyOn(bridge, "enrollment_create_owner").mockRejectedValue({ code: "instance_already_claimed" });
    renderLanding({ fixedOrigin: "https://community.example" });

    fireEvent.click(await screen.findByRole("button", { name: "Start over" }));
    fireEvent.click(screen.getByRole("button", { name: "Discard and start over" }));
    await screen.findByRole("alert");
    fireEvent.click(screen.getByRole("button", { name: "Retry" }));
    fireEvent.click(await screen.findByRole("button", { name: "Create server" }));

    await waitFor(() => expect(bridge.enrollment_create_owner).toHaveBeenCalledOnce());
    const alert = await screen.findByRole("alert");
    expect(alert).toHaveTextContent(peppyCopy.enrollment_error_already_claimed);
    expect(alert).not.toHaveTextContent(peppyCopy.setup_owner_resume_claimed);
    expect(document.getElementById("enrollment-error-code")).toHaveAttribute("data-error-code", "instance_already_claimed");
  });

  it("shows hosted-neutral copy and a safe billing link only with account metadata", async () => {
    renderLanding({ fixedOrigin: "https://app.example.com", accountUrl: "https://account.example.com/account" });

    const link = await screen.findByRole("link", { name: /manage account & billing/i });
    expect(document.querySelector("#self-hosted-panel > h2")).toHaveTextContent("Connect this device");
    expect(link).toHaveAttribute("href", "https://account.example.com/account");
    expect(link).toHaveAttribute("target", "_blank");
    expect(link).toHaveAttribute("rel", "noopener noreferrer");
  });

  it("starts hosted enrollment only after the existing-phone choice with the native origin", async () => {
    vi.spyOn(bridge, "enrollment_discover").mockImplementation(async origin => {
      if (!origin) throw { code: "server-required" };
      return waitingView({ mode: "hosted", origin });
    });
    renderLanding();
    await chooseExisting();
    await waitFor(() => expect(bridge.enrollment_discover).toHaveBeenCalledWith("https://peppy.pro"));
    expect(await screen.findByRole("img", { name: /pairing qr/i })).toBeInTheDocument();
    expect(document.body).not.toHaveTextContent("secret-qr-payload");
  });

  it("uses the native loopback hosted origin for existing-device discovery", async () => {
    Object.assign(bridge, { hosted_origin_url: vi.fn().mockResolvedValue("http://127.0.0.1:8787") });
    renderLanding();

    await chooseExisting();

    await waitFor(() => expect(bridge.enrollment_discover).toHaveBeenCalledWith("http://127.0.0.1:8787"));
    expect(bridge.enrollment_discover).not.toHaveBeenCalledWith("https://peppy.pro");
  });

  it("does not discover after Back while the native hosted origin is pending", async () => {
    const origin = deferred<string>();
    Object.assign(bridge, { hosted_origin_url: vi.fn().mockReturnValue(origin.promise) });
    renderLanding();

    await chooseExisting();
    fireEvent.click(await screen.findByRole("button", { name: "Back" }));
    await act(async () => origin.resolve("http://127.0.0.1:8787"));

    expect(bridge.enrollment_discover).not.toHaveBeenCalled();
    expect(screen.getByRole("button", { name: /already use peppy/i })).toBeInTheDocument();
  });

  it("retries a native hosted-origin failure without leaving the existing-device path", async () => {
    Object.assign(bridge, {
      hosted_origin_url: vi.fn()
        .mockRejectedValueOnce({ code: "offline" })
        .mockResolvedValueOnce("http://127.0.0.1:8787"),
    });
    renderLanding();

    await chooseExisting();
    fireEvent.click(await screen.findByRole("button", { name: "Retry" }));

    await waitFor(() => expect(bridge.enrollment_discover).toHaveBeenCalledWith("http://127.0.0.1:8787"));
    expect(screen.queryByRole("button", { name: /already use peppy/i })).not.toBeInTheDocument();
  });

  it("retries hosted discovery with the production origin", async () => {
    vi.spyOn(bridge, "enrollment_discover")
      .mockRejectedValueOnce({ code: "server-required" })
      .mockResolvedValueOnce(waitingView({ mode: "hosted", origin: "https://peppy.pro" }));
    renderLanding();
    await chooseExisting();
    fireEvent.click(await screen.findByRole("button", { name: "Retry" }));

    await waitFor(() => expect(bridge.enrollment_discover).toHaveBeenNthCalledWith(2, "https://peppy.pro"));
    expect(await screen.findByRole("img", { name: /pairing qr/i })).toBeInTheDocument();
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
    expect(bridge.enrollment_discover).toHaveBeenCalledWith("https://peppy.pro");
    expect(document.getElementById("hosted-path-chooser")).not.toBeInTheDocument();
  });

  it("routes a newly signed-in existing-vault account into hosted enrollment", async () => {
    vi.spyOn(bridge, "hosted_sign_in").mockResolvedValue(account({ signedIn: true, hasVault: true, access: "read_write" }));
    renderLanding();
    fireEvent.click(await screen.findByRole("button", { name: /i'm new to peppy/i }));
    fireEvent.click(await screen.findByRole("button", { name: /continue with google/i }));
    await waitFor(() => expect(bridge.enrollment_discover).toHaveBeenCalledWith("https://peppy.pro"));
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
    await waitFor(() => expect(bridge.enrollment_discover).toHaveBeenCalled());
    view1.rerender(<SetupLanding mode="self-hosted" onMode={vi.fn()} enrolledWithoutPhone={false} onJoined={vi.fn()} />);
    expect(screen.getByLabelText("Server URL")).toBeInTheDocument();
    view1.rerender(<SetupLanding mode="hosted" onMode={vi.fn()} enrolledWithoutPhone={false} onJoined={vi.fn()} />);
    expect(await screen.findByRole("button", { name: /already use peppy/i })).toBeInTheDocument();
    expect(bridge.enrollment_cancel).toHaveBeenCalled();
  });

  it("creates a fresh hosted vault without a passphrase acknowledgement or secret field", async () => {
    vi.spyOn(bridge, "hosted_account").mockResolvedValue(account({ signedIn: true, access: "read_write" }));
    renderLanding();
    const create = await screen.findByRole("button", { name: "Create my vault" });
    expect(create).toBeEnabled();
    expect(screen.queryByRole("checkbox")).not.toBeInTheDocument();
    expect(document.querySelector("input[type=password]")).not.toBeInTheDocument();
    fireEvent.click(create);
    await waitFor(() => expect(bridge.hosted_provision).toHaveBeenCalledOnce());
  });

  it("disables fresh vault creation while provisioning is in flight", async () => {
    const provision = deferred<void>();
    vi.spyOn(bridge, "hosted_account").mockResolvedValue(account({ signedIn: true, access: "read_write" }));
    vi.spyOn(bridge, "hosted_provision").mockReturnValue(provision.promise);
    renderLanding();
    const create = await screen.findByRole("button", { name: "Create my vault" });
    fireEvent.click(create);
    await waitFor(() => expect(create).toBeDisabled());
    expect(screen.getByText(peppyCopy.hosted_create_vault_progress)).toBeInTheDocument();
    fireEvent.click(create);
    expect(bridge.hosted_provision).toHaveBeenCalledOnce();
    await act(async () => provision.resolve());
    await waitFor(() => expect(create).toBeEnabled());
  });

  it("shows a sanitized diagnostic code on fresh and resumable provisioning failures", async () => {
    vi.spyOn(bridge, "hosted_account").mockResolvedValue(account({ signedIn: true, access: "read_write" }));
    vi.spyOn(bridge, "hosted_provision").mockRejectedValueOnce({ code: "secure-store-unavailable" });
    renderLanding();
    fireEvent.click(await screen.findByRole("button", { name: "Create my vault" }));
    expect(await screen.findByRole("alert")).not.toHaveTextContent("secure-store-unavailable");
    expect(document.getElementById("hosted-passphrase-error-code")).toHaveAttribute("data-error-code", "secure-store-unavailable");
    expect(document.getElementById("hosted-passphrase-error-code")).toHaveTextContent("Error code: secure-store-unavailable");
    cleanup();

    vi.spyOn(bridge, "hosted_account").mockResolvedValue(account({ signedIn: true, access: "read_write", resumable: true }));
    vi.spyOn(bridge, "hosted_provision").mockRejectedValueOnce({ code: "network" });
    renderLanding();
    expect(await screen.findByRole("alert")).not.toHaveTextContent("network");
    expect(document.getElementById("hosted-provision-error-code")).toHaveAttribute("data-error-code", "network");
    expect(document.getElementById("hosted-provision-error-code")).toHaveTextContent("Error code: network");
  });

  it("never renders raw messages, payloads, or unknown codes from a failed provision", async () => {
    const leaked = {
      code: "PASSPHRASE-LEAK-CODE",
      message: "correct horse battery staple passphrase-leak",
      body: "{\"vault_secret\":\"body-leak\"}",
      details: "details-leak",
    };
    vi.spyOn(bridge, "hosted_account").mockResolvedValue(account({ signedIn: true, access: "read_write" }));
    vi.spyOn(bridge, "hosted_provision").mockRejectedValueOnce(leaked);
    renderLanding();
    fireEvent.click(await screen.findByRole("button", { name: "Create my vault" }));
    expect(await screen.findByRole("alert")).toBeInTheDocument();
    expect(document.getElementById("hosted-passphrase-error-code")).toHaveAttribute("data-error-code", "unavailable");
    expect(document.body).not.toHaveTextContent("PASSPHRASE-LEAK-CODE");
    expect(document.body).not.toHaveTextContent("correct horse");
    expect(document.body).not.toHaveTextContent("body-leak");
    expect(document.body).not.toHaveTextContent("details-leak");
    cleanup();

    vi.spyOn(bridge, "hosted_account").mockResolvedValue(account({ signedIn: true, access: "read_write", resumable: true }));
    vi.spyOn(bridge, "hosted_provision").mockRejectedValueOnce(new Error("correct horse battery staple passphrase-leak"));
    renderLanding();
    expect(await screen.findByRole("alert")).toBeInTheDocument();
    expect(document.getElementById("hosted-provision-error-code")).toHaveAttribute("data-error-code", "unavailable");
    expect(document.body).not.toHaveTextContent("correct horse");
  });

  it("shows the creation error on the fresh card and retries provisioning", async () => {
    vi.spyOn(bridge, "hosted_account").mockResolvedValue(account({ signedIn: true, access: "read_write" }));
    vi.spyOn(bridge, "hosted_provision").mockRejectedValueOnce({ code: "network" }).mockResolvedValueOnce();
    renderLanding();
    fireEvent.click(await screen.findByRole("button", { name: "Create my vault" }));
    expect(await screen.findByRole("alert")).toHaveTextContent(peppyCopy.hosted_create_vault_error);
    expect(document.getElementById("hosted-passphrase-error")).toBeInTheDocument();
    fireEvent.click(screen.getByRole("button", { name: "Try again" }));
    await waitFor(() => expect(bridge.hosted_provision).toHaveBeenCalledTimes(2));
    await waitFor(() => expect(screen.queryByRole("alert")).not.toBeInTheDocument());
  });

  it("automatically creates a transfer invitation for an enrolled owner without a phone", async () => {
    renderLanding({ enrolledWithoutPhone: true });
    expect(await screen.findByRole("heading", { name: /connect your phone/i })).toBeInTheDocument();
    await waitFor(() => expect(bridge.enrollment_add_phone).toHaveBeenCalledWith("transfer"));
    expect(await screen.findByRole("img", { name: /pairing qr/i })).toBeInTheDocument();
    expect(screen.queryByLabelText("Server mode")).not.toBeInTheDocument();
    expect(bridge.enrollment_status).toHaveBeenCalled();
    expect(bridge.hosted_account).not.toHaveBeenCalled();
  });

  it("keeps the phone heading when explicitly adding a phone", async () => {
    renderLanding({ addingPhone: true });
    expect(await screen.findByRole("heading", { name: "Connect your phone" })).toBeInTheDocument();
  });

  it("keeps cancellation pending until the native cancellation settles and does not recreate the invitation", async () => {
    const cancellation = deferred<void>();
    vi.spyOn(bridge, "enrollment_discover").mockResolvedValue(waitingView());
    vi.spyOn(bridge, "enrollment_cancel").mockReturnValue(cancellation.promise);
    renderLanding({ fixedOrigin: "https://community.example", addingPhone: true });

    fireEvent.click(await screen.findByRole("button", { name: "Cancel" }));
    expect(screen.getByRole("button", { name: "Cancel" })).toBeDisabled();
    expect(document.getElementById("setup-pending-operation")).toHaveTextContent(peppyCopy.production_pairing_cancelling);
    await act(async () => cancellation.resolve());
    expect(bridge.enrollment_add_phone).not.toHaveBeenCalled();
  });

  it("waits for confirmed connection after approval and never restarts an approved invitation", async () => {
    const advance = capturePoll();
    vi.spyOn(bridge, "enrollment_discover").mockResolvedValue(view({ stage: "phone_approved", owner: true }));
    vi.spyOn(bridge, "enrollment_status").mockResolvedValue(view({ stage: "phone_approved", owner: true }));
    renderLanding({ fixedOrigin: "https://community.example" });

    expect(await screen.findByText(/phone approved/i)).toBeInTheDocument();
    await advance();
    expect(bridge.enrollment_rotate_phone).not.toHaveBeenCalled();
    expect(bridge.enrollment_add_phone).not.toHaveBeenCalled();
  });

  it("keeps completion pending through a synchronous callback failure and retries safely", async () => {
    const onJoined = vi.fn().mockImplementationOnce(() => { throw { code: "offline" }; }).mockResolvedValueOnce(undefined);
    vi.spyOn(bridge, "enrollment_discover").mockResolvedValue(view({ stage: "phone_connected", owner: true }));
    renderLanding({ fixedOrigin: "https://community.example", onJoined });

    expect(await screen.findByRole("button", { name: "Retry" })).toBeEnabled();
    expect(onJoined).toHaveBeenCalledOnce();
    fireEvent.click(screen.getByRole("button", { name: "Retry" }));
    await waitFor(() => expect(onJoined).toHaveBeenCalledTimes(2));
  });

  it("keeps deferral pending and actions disabled while the parent callback is still pending after cancel resolves", async () => {
    const parent = deferred<void>();
    const cancel = deferred<EnrollmentView>();
    const onDeferPhone = vi.fn(() => parent.promise);
    vi.spyOn(bridge, "enrollment_cancel_phone").mockReturnValue(cancel.promise);
    renderLanding({ enrolledWithoutPhone: true, onDeferPhone });

    fireEvent.click(await screen.findByRole("button", { name: "Set up phone later" }));
    expect(document.getElementById("setup-pending-operation")).toHaveTextContent(peppyCopy.production_pairing_cancelling);
    await act(async () => cancel.resolve(view({ stage: "ready", owner: true })));
    await waitFor(() => expect(onDeferPhone).toHaveBeenCalledOnce());
    expect(document.getElementById("setup-pending-operation")).toHaveTextContent(peppyCopy.production_pairing_cancelling);
    expect(screen.getByRole("button", { name: "Set up phone later" })).toBeDisabled();
    expect(screen.getByRole("button", { name: "Cancel" })).toBeDisabled();

    await act(async () => parent.resolve());
    await waitFor(() => expect(document.getElementById("setup-pending-operation")).not.toBeInTheDocument());
  });

  it("retries the deferral after a rejected native cancellation and calls the parent only once it succeeds", async () => {
    const onDeferPhone = vi.fn();
    vi.spyOn(bridge, "enrollment_cancel_phone").mockRejectedValueOnce({ code: "offline" });
    renderLanding({ enrolledWithoutPhone: true, onDeferPhone });

    fireEvent.click(await screen.findByRole("button", { name: "Set up phone later" }));
    fireEvent.click(await screen.findByRole("button", { name: "Retry" }));
    await waitFor(() => expect(onDeferPhone).toHaveBeenCalledOnce());
    expect(bridge.enrollment_cancel_phone).toHaveBeenCalledTimes(2);
  });

  it("shows the error and Retry without calling the parent when native cancellation rejects", async () => {
    const onDeferPhone = vi.fn();
    vi.spyOn(bridge, "enrollment_cancel_phone").mockRejectedValue({ code: "offline" });
    renderLanding({ enrolledWithoutPhone: true, onDeferPhone });

    fireEvent.click(await screen.findByRole("button", { name: "Set up phone later" }));
    expect(await screen.findByRole("button", { name: "Retry" })).toBeEnabled();
    expect(document.getElementById("enrollment-error-code")).toHaveAttribute("data-error-code", "offline");
    expect(onDeferPhone).not.toHaveBeenCalled();
  });

  it("does not call the parent deferral when the landing unmounts during native cancellation", async () => {
    const cancel = deferred<EnrollmentView>();
    const onDeferPhone = vi.fn();
    vi.spyOn(bridge, "enrollment_cancel_phone").mockReturnValue(cancel.promise);
    const rendered = renderLanding({ enrolledWithoutPhone: true, onDeferPhone });

    fireEvent.click(await screen.findByRole("button", { name: "Set up phone later" }));
    await waitFor(() => expect(bridge.enrollment_cancel_phone).toHaveBeenCalledOnce());
    rendered.unmount();
    await act(async () => cancel.resolve(view({ stage: "ready", owner: true })));

    expect(onDeferPhone).not.toHaveBeenCalled();
  });

  it("does not call the parent deferral when the server mode changes during native cancellation", async () => {
    const cancel = deferred<EnrollmentView>();
    const onDeferPhone = vi.fn();
    vi.spyOn(bridge, "enrollment_cancel_phone").mockReturnValue(cancel.promise);
    const rendered = renderLanding({ mode: "self-hosted", onDeferPhone });
    fireEvent.change(screen.getByLabelText("Server URL"), { target: { value: "https://server.example" } });
    fireEvent.click(screen.getByRole("button", { name: "Connect" }));
    fireEvent.click(await screen.findByRole("button", { name: "Set up phone later" }));
    await waitFor(() => expect(bridge.enrollment_cancel_phone).toHaveBeenCalledOnce());

    rendered.rerender(<SetupLanding mode="hosted" onMode={vi.fn()} enrolledWithoutPhone={false} onDeferPhone={onDeferPhone} onJoined={vi.fn()} />);
    await act(async () => cancel.resolve(view({ stage: "ready", owner: true })));

    expect(onDeferPhone).not.toHaveBeenCalled();
    expect(document.getElementById("setup-pending-operation")).not.toBeInTheDocument();
  });

  it("retries a rejected parent deferral through Retry", async () => {
    const onDeferPhone = vi.fn().mockRejectedValueOnce({ code: "offline" }).mockResolvedValueOnce(undefined);
    renderLanding({ enrolledWithoutPhone: true, onDeferPhone });

    fireEvent.click(await screen.findByRole("button", { name: "Set up phone later" }));
    fireEvent.click(await screen.findByRole("button", { name: "Retry" }));
    await waitFor(() => expect(onDeferPhone).toHaveBeenCalledTimes(2));
    await waitFor(() => expect(document.getElementById("setup-pending-operation")).not.toBeInTheDocument());
  });

  it("shows one checking status while the initial owner reconciliation is pending", async () => {
    const status = deferred<EnrollmentView>();
    vi.spyOn(bridge, "enrollment_status").mockReturnValue(status.promise);
    renderLanding({ enrolledWithoutPhone: true });

    expect(screen.getAllByText(peppyCopy.production_pairing_checking)).toHaveLength(1);
    await act(async () => status.resolve(view({ stage: "ready", owner: true, hasGateway: false })));
  });

  it.each(["setup", "signin"] as const)("rechecks a stale owner %s status until it can create a phone invitation", async (staleStage) => {
    vi.useFakeTimers();
    vi.spyOn(bridge, "enrollment_status")
      .mockResolvedValueOnce(view({ stage: staleStage }))
      .mockResolvedValueOnce(view({ stage: staleStage }))
      .mockResolvedValueOnce(view({ stage: "ready", owner: true, hasGateway: false }));
    renderLanding({ enrolledWithoutPhone: true });

    await act(async () => {});
    expect(screen.queryByRole("button", { name: "Create server" })).not.toBeInTheDocument();
    await act(async () => { await vi.advanceTimersByTimeAsync(1_000); });
    await act(async () => { await vi.advanceTimersByTimeAsync(1_000); });

    expect(bridge.enrollment_status).toHaveBeenCalledTimes(3);
    expect(screen.getByRole("heading", { name: /connect your phone/i })).toBeInTheDocument();
    expect(bridge.enrollment_add_phone).toHaveBeenCalledWith("transfer");
    vi.useRealTimers();
    expect(await screen.findByRole("img", { name: /pairing qr/i })).toBeInTheDocument();
  });

  it("reports a retryable error when owner status stays stale", async () => {
    vi.useFakeTimers();
    vi.spyOn(bridge, "enrollment_status").mockResolvedValue(view({ stage: "setup" }));
    renderLanding({ enrolledWithoutPhone: true });

    await act(async () => {});
    for (let attempt = 0; attempt < 15; attempt += 1) {
      await act(async () => { await vi.advanceTimersByTimeAsync(1_000); });
    }

    expect(screen.getByRole("button", { name: "Retry" })).toBeEnabled();
    expect(document.getElementById("enrollment-error-code")).toHaveAttribute("data-error-code", "owner-status-pending");
    expect(document.getElementById("setup-owner-status-checking")).not.toBeInTheDocument();
    expect(bridge.enrollment_add_phone).not.toHaveBeenCalled();
  });

  it("continues to show setup for an unclaimed self-hosted server", async () => {
    vi.spyOn(bridge, "enrollment_discover").mockResolvedValue(view({ stage: "setup" }));
    renderLanding({ mode: "self-hosted", fixedOrigin: "https://server.example" });

    expect(await screen.findByRole("button", { name: "Create server" })).toBeInTheDocument();
  });

  it("retries an initial rejected status request with foreground busy feedback", async () => {
    const retryStatus = deferred<EnrollmentView>();
    vi.spyOn(bridge, "enrollment_status")
      .mockRejectedValueOnce({ code: "offline" })
      .mockReturnValueOnce(retryStatus.promise)
      .mockRejectedValueOnce({ code: "offline" })
      .mockResolvedValueOnce(view({ stage: "ready", owner: false }));
    renderLanding({ enrolledWithoutPhone: true });

    fireEvent.click(await screen.findByRole("button", { name: "Retry" }));
    expect(screen.getByRole("button", { name: "Retry" })).toBeDisabled();
    fireEvent.click(screen.getByRole("button", { name: "Retry" }));
    expect(bridge.enrollment_status).toHaveBeenCalledTimes(2);

    await act(async () => retryStatus.reject({ code: "offline" }));
    const retry = await screen.findByRole("button", { name: "Retry" });
    expect(retry).toBeEnabled();
    fireEvent.click(retry);
    await waitFor(() => expect(bridge.enrollment_status).toHaveBeenCalledTimes(3));
    fireEvent.click(await screen.findByRole("button", { name: "Retry" }));
    expect(await screen.findByRole("heading", { name: "Waiting for an owner" })).toBeInTheDocument();
    expect(bridge.enrollment_status).toHaveBeenCalledTimes(4);
  });

  it("retries an initial returned status error without starting discovery", async () => {
    vi.spyOn(bridge, "enrollment_status")
      .mockResolvedValueOnce(view({ stage: "error", errorCode: "offline" }))
      .mockResolvedValueOnce(view({ stage: "ready", owner: false }));
    renderLanding({ enrolledWithoutPhone: true });

    fireEvent.click(await screen.findByRole("button", { name: "Retry" }));

    await waitFor(() => expect(bridge.enrollment_status).toHaveBeenCalledTimes(2));
    expect(await screen.findByRole("heading", { name: "Waiting for an owner" })).toBeInTheDocument();
    expect(bridge.enrollment_discover).not.toHaveBeenCalled();
  });

  it("ignores a successful status retry after a mode reset without restarting polling", async () => {
    const retryStatus = deferred<EnrollmentView>();
    const setInterval = vi.spyOn(window, "setInterval");
    vi.spyOn(bridge, "enrollment_status")
      .mockRejectedValueOnce({ code: "offline" })
      .mockReturnValueOnce(retryStatus.promise);
    const rendered = renderLanding({ mode: "self-hosted", enrolledWithoutPhone: true });

    fireEvent.click(await screen.findByRole("button", { name: "Retry" }));
    rendered.rerender(<SetupLanding mode="hosted" onMode={vi.fn()} enrolledWithoutPhone onJoined={vi.fn()} />);
    await act(async () => retryStatus.resolve(waitingView()));

    expect(document.getElementById("enrollment-waiting_phone")).not.toBeInTheDocument();
    expect(setInterval.mock.calls.some(([, timeout]) => timeout === 4_000)).toBe(false);
  });

  it("ignores a rejected status retry after a mode reset", async () => {
    const retryStatus = deferred<EnrollmentView>();
    vi.spyOn(bridge, "enrollment_status")
      .mockRejectedValueOnce({ code: "offline" })
      .mockReturnValueOnce(retryStatus.promise);
    const rendered = renderLanding({ mode: "self-hosted", enrolledWithoutPhone: true });

    fireEvent.click(await screen.findByRole("button", { name: "Retry" }));
    rendered.rerender(<SetupLanding mode="hosted" onMode={vi.fn()} enrolledWithoutPhone onJoined={vi.fn()} />);
    await act(async () => retryStatus.reject({ code: "offline" }));

    expect(document.getElementById("enrollment-error")).not.toBeInTheDocument();
  });

  it("keeps a newer enrollment mutation busy when an earlier status retry resolves", async () => {
    const retryStatus = deferred<EnrollmentView>();
    const discovery = deferred<EnrollmentView>();
    vi.spyOn(bridge, "enrollment_status")
      .mockRejectedValueOnce({ code: "offline" })
      .mockReturnValueOnce(retryStatus.promise);
    vi.spyOn(bridge, "enrollment_discover").mockReturnValue(discovery.promise);
    const rendered = renderLanding({ enrolledWithoutPhone: true });

    fireEvent.click(await screen.findByRole("button", { name: "Retry" }));
    rendered.rerender(<SetupLanding mode="self-hosted" onMode={vi.fn()} enrolledWithoutPhone={false} onJoined={vi.fn()} />);
    fireEvent.change(await screen.findByLabelText("Server URL"), { target: { value: "https://server.example" } });
    fireEvent.click(screen.getByRole("button", { name: "Connect" }));
    expect(screen.getByRole("button", { name: "Connect" })).toBeDisabled();

    await act(async () => retryStatus.resolve(view({ stage: "ready", owner: false })));

    expect(screen.getByRole("button", { name: "Connect" })).toBeDisabled();
  });

  it("discovers before adding a phone when status starts at server", async () => {
    vi.spyOn(bridge, "enrollment_status").mockResolvedValue(view({ stage: "server" }));
    vi.spyOn(bridge, "enrollment_discover").mockResolvedValue(view({ stage: "ready", owner: true, hasGateway: false }));
    renderLanding({ enrolledWithoutPhone: true });

    await waitFor(() => expect(bridge.enrollment_discover).toHaveBeenCalled());
    await waitFor(() => expect(bridge.enrollment_add_phone).toHaveBeenCalledWith("transfer"));
  });

  it("reconciles a stale claim result after the latest route becomes owner without a phone", async () => {
    const creation = deferred<EnrollmentView>();
    const status = deferred<EnrollmentView>();
    vi.spyOn(bridge, "enrollment_discover").mockResolvedValue(view({ stage: "setup" }));
    vi.spyOn(bridge, "enrollment_create_owner").mockReturnValue(creation.promise);
    vi.spyOn(bridge, "enrollment_status").mockReturnValue(status.promise);
    const rendered = renderLanding({ mode: "self-hosted" });
    fireEvent.change(screen.getByLabelText("Server URL"), { target: { value: "https://server.example" } });
    fireEvent.click(screen.getByRole("button", { name: "Connect" }));
    fireEvent.click(await screen.findByRole("button", { name: "Create server" }));
    rendered.rerender(<SetupLanding mode="self-hosted" onMode={vi.fn()} enrolledWithoutPhone onJoined={vi.fn()} />);
    expect(bridge.enrollment_status).not.toHaveBeenCalled();
    await act(async () => creation.resolve(view({ stage: "setup" })));
    await waitFor(() => expect(bridge.enrollment_status).toHaveBeenCalledOnce());
    expect(screen.getByRole("button", { name: "Create server" })).toBeDisabled();
    await act(async () => status.resolve(view({ stage: "ready", owner: true, hasGateway: false })));
    await waitFor(() => expect(bridge.enrollment_add_phone).toHaveBeenCalledWith("transfer"));
    expect(await screen.findByRole("img", { name: /pairing qr/i })).toBeInTheDocument();
  });

  it("reconciles a stale create-owner result without an enrolled-without-phone route", async () => {
    vi.spyOn(bridge, "enrollment_discover").mockResolvedValue(view({ stage: "setup" }));
    vi.spyOn(bridge, "enrollment_create_owner").mockResolvedValue(view({ stage: "setup" }));
    renderLanding({ mode: "self-hosted" });

    fireEvent.change(screen.getByLabelText("Server URL"), { target: { value: "https://server.example" } });
    fireEvent.click(screen.getByRole("button", { name: "Connect" }));
    fireEvent.click(await screen.findByRole("button", { name: "Create server" }));

    await waitFor(() => expect(bridge.enrollment_status).toHaveBeenCalledOnce());
  });

  it("restores Create server when a cancelled native prompt reconciles to setup", async () => {
    const status = deferred<EnrollmentView>();
    vi.spyOn(bridge, "enrollment_discover").mockResolvedValue(view({ stage: "setup" }));
    vi.spyOn(bridge, "enrollment_create_owner").mockResolvedValue(view({ stage: "setup" }));
    vi.spyOn(bridge, "enrollment_status").mockReturnValue(status.promise);
    renderLanding({ mode: "self-hosted" });

    fireEvent.change(screen.getByLabelText("Server URL"), { target: { value: "https://server.example" } });
    fireEvent.click(screen.getByRole("button", { name: "Connect" }));
    fireEvent.click(await screen.findByRole("button", { name: "Create server" }));

    await waitFor(() => expect(bridge.enrollment_status).toHaveBeenCalledOnce());
    expect(screen.getByRole("button", { name: "Create server" })).toBeDisabled();
    await act(async () => status.resolve(view({ stage: "setup" })));
    await waitFor(() => expect(screen.getByRole("button", { name: "Create server" })).toBeEnabled());
  });

  it("keeps a stale claim result busy until status confirms a connected gateway", async () => {
    const creation = deferred<EnrollmentView>();
    const status = deferred<EnrollmentView>();
    vi.spyOn(bridge, "enrollment_discover").mockResolvedValue(view({ stage: "setup" }));
    vi.spyOn(bridge, "enrollment_create_owner").mockReturnValue(creation.promise);
    vi.spyOn(bridge, "enrollment_status").mockReturnValue(status.promise);
    const rendered = renderLanding({ mode: "self-hosted" });
    fireEvent.change(screen.getByLabelText("Server URL"), { target: { value: "https://server.example" } });
    fireEvent.click(screen.getByRole("button", { name: "Connect" }));
    fireEvent.click(await screen.findByRole("button", { name: "Create server" }));
    rendered.rerender(<SetupLanding mode="self-hosted" onMode={vi.fn()} enrolledWithoutPhone onJoined={vi.fn()} />);
    await act(async () => creation.resolve(view({ stage: "setup" })));

    await waitFor(() => expect(bridge.enrollment_status).toHaveBeenCalledOnce());
    expect(screen.getByRole("button", { name: "Create server" })).toBeDisabled();
    await act(async () => status.resolve(view({ stage: "ready", owner: true, hasGateway: true })));
    expect(await screen.findByText("Phone connected.")).toBeInTheDocument();
    expect(bridge.enrollment_add_phone).not.toHaveBeenCalled();
  });

  it("keeps foreground claim reconciliation responsible when route status refreshes", async () => {
    const creation = deferred<EnrollmentView>();
    const status = deferred<EnrollmentView>();
    vi.spyOn(bridge, "enrollment_discover").mockResolvedValue(view({ stage: "setup" }));
    vi.spyOn(bridge, "enrollment_create_owner").mockReturnValue(creation.promise);
    vi.spyOn(bridge, "enrollment_status").mockReturnValue(status.promise);
    const onDeferPhone = vi.fn();
    const rendered = renderLanding({ mode: "self-hosted", onDeferPhone });
    fireEvent.change(screen.getByLabelText("Server URL"), { target: { value: "https://server.example" } });
    fireEvent.click(screen.getByRole("button", { name: "Connect" }));
    fireEvent.click(await screen.findByRole("button", { name: "Create server" }));
    rendered.rerender(<SetupLanding mode="self-hosted" onMode={vi.fn()} enrolledWithoutPhone onDeferPhone={onDeferPhone} onJoined={vi.fn()} />);
    await act(async () => creation.resolve(view({ stage: "setup" })));
    await waitFor(() => expect(bridge.enrollment_status).toHaveBeenCalledOnce());
    rendered.rerender(<SetupLanding mode="self-hosted" onMode={vi.fn()} enrolledWithoutPhone={false} onDeferPhone={onDeferPhone} onJoined={vi.fn()} />);
    rendered.rerender(<SetupLanding mode="self-hosted" onMode={vi.fn()} enrolledWithoutPhone onDeferPhone={onDeferPhone} onJoined={vi.fn()} />);

    expect(bridge.enrollment_status).toHaveBeenCalledOnce();
    await act(async () => status.resolve(view({ stage: "ready", owner: true })));
    expect(await screen.findByRole("button", { name: "Set up phone later" })).toBeEnabled();
    expect(bridge.enrollment_add_phone).not.toHaveBeenCalled();
  });

  it("retries unknown gateway status until known absence before automatically creating a phone invitation", async () => {
    vi.useFakeTimers();
    vi.spyOn(bridge, "enrollment_status")
      .mockResolvedValueOnce(view({ stage: "ready", owner: true }))
      .mockResolvedValueOnce(view({ stage: "ready", owner: true }))
      .mockResolvedValueOnce(view({ stage: "ready", owner: true, hasGateway: false }));
    renderLanding({ enrolledWithoutPhone: true });

    await act(async () => undefined);
    expect(bridge.enrollment_status).toHaveBeenCalledOnce();
    expect(bridge.enrollment_add_phone).not.toHaveBeenCalled();
    await act(async () => vi.advanceTimersByTime(4_000));
    expect(bridge.enrollment_status).toHaveBeenCalledTimes(2);
    expect(bridge.enrollment_add_phone).not.toHaveBeenCalled();
    await act(async () => vi.advanceTimersByTime(4_000));
    expect(bridge.enrollment_status).toHaveBeenCalledTimes(3);
    expect(bridge.enrollment_add_phone).toHaveBeenCalledWith("transfer");
    expect(bridge.enrollment_add_phone).toHaveBeenCalledOnce();
  });

  it("skips automatic phone setup when status reports a connected gateway", async () => {
    vi.spyOn(bridge, "enrollment_status").mockResolvedValue(view({ stage: "ready", owner: true, hasGateway: true }));
    renderLanding({ enrolledWithoutPhone: true });

    await waitFor(() => expect(bridge.enrollment_status).toHaveBeenCalledOnce());
    expect(bridge.enrollment_add_phone).not.toHaveBeenCalled();
  });

  it("retries unknown gateway status until a connected gateway is known", async () => {
    vi.useFakeTimers();
    vi.spyOn(bridge, "enrollment_status")
      .mockResolvedValueOnce(view({ stage: "ready", owner: true }))
      .mockResolvedValueOnce(view({ stage: "ready", owner: true }))
      .mockResolvedValueOnce(view({ stage: "ready", owner: true, hasGateway: true }));
    renderLanding({ enrolledWithoutPhone: true });

    await act(async () => undefined);
    await act(async () => vi.advanceTimersByTime(4_000));
    await act(async () => vi.advanceTimersByTime(4_000));
    expect(bridge.enrollment_status).toHaveBeenCalledTimes(3);
    expect(bridge.enrollment_add_phone).not.toHaveBeenCalled();
  });

  it("ignores an older absent-gateway status after newer status finds a connected phone", async () => {
    vi.useFakeTimers();
    const olderStatus = deferred<EnrollmentView>();
    const newerStatus = deferred<EnrollmentView>();
    vi.spyOn(bridge, "enrollment_status")
      .mockResolvedValueOnce(view({ stage: "ready", owner: true }))
      .mockReturnValueOnce(olderStatus.promise)
      .mockReturnValueOnce(newerStatus.promise);
    const rendered = renderLanding({ enrolledWithoutPhone: true });

    await act(async () => undefined);
    await act(async () => vi.advanceTimersByTime(4_000));
    expect(bridge.enrollment_status).toHaveBeenCalledTimes(2);
    rendered.rerender(<SetupLanding mode="hosted" onMode={vi.fn()} enrolledWithoutPhone={false} onJoined={vi.fn()} />);
    rendered.rerender(<SetupLanding mode="hosted" onMode={vi.fn()} enrolledWithoutPhone onJoined={vi.fn()} />);
    await act(async () => undefined);
    expect(bridge.enrollment_status).toHaveBeenCalledTimes(3);

    await act(async () => newerStatus.resolve(view({ stage: "ready", owner: true, hasGateway: true })));
    await act(async () => olderStatus.resolve(view({ stage: "ready", owner: true, hasGateway: false })));
    expect(bridge.enrollment_add_phone).not.toHaveBeenCalled();
  });

  it("lets an owner defer phone setup while gateway status is unknown", async () => {
    const onDeferPhone = vi.fn();
    vi.spyOn(bridge, "enrollment_status").mockResolvedValue(view({ stage: "ready", owner: true }));
    vi.spyOn(bridge, "enrollment_cancel_phone").mockResolvedValue(view({ stage: "ready", owner: true }));
    renderLanding({ enrolledWithoutPhone: true, onDeferPhone });

    fireEvent.click(await screen.findByRole("button", { name: "Set up phone later" }));
    await waitFor(() => expect(bridge.enrollment_cancel_phone).toHaveBeenCalledOnce());
    expect(onDeferPhone).toHaveBeenCalledOnce();
    expect(bridge.enrollment_add_phone).not.toHaveBeenCalled();
  });

  it("reconciles a remounted owner setup through status before adding a phone", async () => {
    const creation = deferred<EnrollmentView>();
    vi.spyOn(bridge, "enrollment_discover").mockResolvedValue(view({ stage: "setup" }));
    vi.spyOn(bridge, "enrollment_create_owner").mockReturnValue(creation.promise);
    const rendered = renderLanding({ mode: "self-hosted" });
    fireEvent.change(screen.getByLabelText("Server URL"), { target: { value: "https://server.example" } });
    fireEvent.click(screen.getByRole("button", { name: "Connect" }));
    fireEvent.click(await screen.findByRole("button", { name: "Create server" }));
    rendered.unmount();
    vi.spyOn(bridge, "enrollment_status").mockResolvedValue(view({ stage: "ready", owner: true, hasGateway: false }));
    renderLanding({ enrolledWithoutPhone: true });

    await waitFor(() => expect(bridge.enrollment_add_phone).toHaveBeenCalledWith("transfer"));
    await act(async () => creation.resolve(view({ stage: "ready", owner: true })));
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
    fireEvent.click(await screen.findByRole("button", { name: "Create my vault" }));
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

  it("provisions a resumable account and a fresh hosted vault", async () => {
    vi.spyOn(bridge, "hosted_account").mockResolvedValue(account({ signedIn: true, access: "read_write", resumable: true }));
    renderLanding();
    await waitFor(() => expect(bridge.hosted_provision).toHaveBeenCalledOnce());
    cleanup();
    vi.spyOn(bridge, "hosted_account").mockResolvedValue(account({ signedIn: true, access: "read_write" }));
    renderLanding();
    fireEvent.click(await screen.findByRole("button", { name: "Create my vault" }));
    await waitFor(() => expect(bridge.hosted_provision).toHaveBeenCalledTimes(2));
  });

  it("records Hosted when an explicit passphrase provision begins", async () => {
    vi.spyOn(bridge, "hosted_account").mockResolvedValue(account({ signedIn: true, access: "read_write" }));
    renderLanding();
    fireEvent.click(await screen.findByRole("button", { name: "Create my vault" }));
    expect(window.localStorage.getItem("peppy.setup.mode")).toBe("hosted");
  });

  it("refreshes the account after a failed fresh vault creation", async () => {
    vi.spyOn(bridge, "hosted_account").mockResolvedValue(account({ signedIn: true, access: "read_write" }));
    vi.spyOn(bridge, "hosted_provision").mockRejectedValue({ code: "network" });
    renderLanding();
    fireEvent.click(await screen.findByRole("button", { name: "Create my vault" }));
    await screen.findByRole("alert");
    await waitFor(() => expect(bridge.hosted_account).toHaveBeenCalledTimes(2));
  });

  it("reauthenticates a hosted enrollment session loss by reconciling status without replaying approval", async () => {
    vi.spyOn(bridge, "hosted_account").mockResolvedValue(account({ signedIn: true, hasVault: true, access: "read_write" }));
    vi.spyOn(bridge, "enrollment_discover").mockResolvedValue(view({ mode: "hosted", stage: "verify_phone", verificationCode: "123456" }));
    vi.spyOn(bridge, "enrollment_approve").mockRejectedValue({ code: "hosted-session-required" });
    vi.spyOn(bridge, "hosted_sign_in").mockResolvedValue(account({ signedIn: true, hasVault: true, access: "read_write" }));
    vi.spyOn(bridge, "enrollment_status").mockResolvedValue(view({ mode: "hosted", stage: "verify_phone", verificationCode: "123456" }));
    renderLanding();

    await screen.findByRole("checkbox");
    fireEvent.click(screen.getByRole("checkbox"));
    fireEvent.click(screen.getByRole("button", { name: "Confirm and approve" }));
    fireEvent.click(await screen.findByRole("button", { name: "Sign in again with Google" }));

    await waitFor(() => expect(bridge.enrollment_status).toHaveBeenCalled());
    await waitFor(() => expect(document.getElementById("hosted-session-recovery-panel")).not.toBeInTheDocument());
    expect(await screen.findByLabelText("Verification code: 123456")).toBeInTheDocument();
    const approve = screen.getByRole("button", { name: "Confirm and approve" });
    expect(approve).toBeDisabled();
    fireEvent.click(screen.getByRole("checkbox", { name: /codes match/i }));
    expect(approve).toBeEnabled();
    expect(bridge.enrollment_approve).toHaveBeenCalledOnce();
  });

  it("resumes a protected provisioning checkpoint once after hosted reauthentication", async () => {
    vi.spyOn(bridge, "hosted_account")
      .mockResolvedValueOnce(account({ signedIn: true, access: "read_write", resumable: true }))
      .mockResolvedValue(account());
    vi.spyOn(bridge, "hosted_provision").mockRejectedValueOnce({ code: "hosted-session-required" }).mockResolvedValueOnce();
    vi.spyOn(bridge, "hosted_sign_in").mockResolvedValue(account({ signedIn: true, access: "read_write", resumable: true }));
    renderLanding();

    fireEvent.click(await screen.findByRole("button", { name: "Sign in again with Google" }));

    await waitFor(() => expect(bridge.hosted_provision).toHaveBeenCalledTimes(2));
    expect(await screen.findByRole("button", { name: /already use peppy/i })).toBeInTheDocument();
    expect(screen.getByRole("button", { name: /i'm new to peppy/i })).toBeInTheDocument();
    await act(async () => { await new Promise(done => setTimeout(done, 20)); });
    expect(bridge.hosted_provision).toHaveBeenCalledTimes(2);
  });

  it("keeps hosted recovery available after a rejected sign-in", async () => {
    vi.spyOn(bridge, "hosted_account").mockResolvedValue(account({ signedIn: true, access: "read_write" }));
    vi.spyOn(bridge, "hosted_provision").mockRejectedValue({ code: "hosted-session-required" });
    vi.spyOn(bridge, "hosted_sign_in").mockRejectedValue(new Error("cancelled"));
    renderLanding();

    fireEvent.click(await screen.findByRole("button", { name: "Create my vault" }));
    const retry = await screen.findByRole("button", { name: "Sign in again with Google" });
    fireEvent.click(retry);

    await waitFor(() => expect(bridge.hosted_sign_in).toHaveBeenCalledOnce());
    await waitFor(() => expect(screen.getByRole("button", { name: "Sign in again with Google" })).toBeEnabled());
    const error = document.getElementById("hosted-session-recovery-error");
    expect(error).toHaveAttribute("role", "alert");
    expect(error).toBeVisible();
    expect(error).toHaveTextContent(peppyCopy.production_signin_failed);
    expect(document.body).not.toHaveTextContent("cancelled");
  });

  it("does not offer account recovery for device credentials or browser and community enrollment", async () => {
    vi.spyOn(bridge, "enrollment_discover").mockResolvedValue(view({ mode: "hosted", stage: "error", errorCode: "credentials-required" }));
    renderLanding();
    await chooseExisting();
    await screen.findByRole("alert");
    expect(screen.queryByRole("button", { name: "Sign in again with Google" })).not.toBeInTheDocument();

    cleanup();
    vi.spyOn(bridge, "enrollment_discover").mockResolvedValue(view({ mode: "community", stage: "error", errorCode: "hosted-session-required" }));
    renderLanding({ fixedOrigin: "https://community.example" });
    await screen.findByRole("alert");
    expect(screen.queryByRole("button", { name: "Sign in again with Google" })).not.toBeInTheDocument();
  });

  it("keeps recovery active when OAuth returns a signed-out account", async () => {
    vi.spyOn(bridge, "hosted_account").mockResolvedValue(account({ signedIn: true, access: "read_write" }));
    vi.spyOn(bridge, "hosted_provision").mockRejectedValue({ code: "hosted-session-required" });
    vi.spyOn(bridge, "hosted_sign_in").mockResolvedValue(account());
    renderLanding();

    fireEvent.click(await screen.findByRole("button", { name: "Create my vault" }));
    fireEvent.click(await screen.findByRole("button", { name: "Sign in again with Google" }));

    await waitFor(() => expect(bridge.hosted_sign_in).toHaveBeenCalledOnce());
    await waitFor(() => expect(screen.getByRole("button", { name: "Sign in again with Google" })).toBeEnabled());
    expect(bridge.hosted_provision).toHaveBeenCalledOnce();
    expect(screen.getByRole("status")).toHaveTextContent("Sign in again to continue.");
  });

  it("starts only one OAuth request while hosted recovery is pending", async () => {
    const signIn = deferred<HostedAccountView>();
    vi.spyOn(bridge, "hosted_account").mockResolvedValue(account({ signedIn: true, access: "read_write" }));
    vi.spyOn(bridge, "hosted_provision").mockRejectedValue({ code: "hosted-session-required" });
    vi.spyOn(bridge, "hosted_sign_in").mockReturnValue(signIn.promise);
    renderLanding();

    fireEvent.click(await screen.findByRole("button", { name: "Create my vault" }));
    const recover = await screen.findByRole("button", { name: "Sign in again with Google" });
    fireEvent.click(recover);
    fireEvent.click(recover);

    expect(recover).toBeDisabled();
    expect(bridge.hosted_sign_in).toHaveBeenCalledOnce();
    await act(async () => signIn.resolve(account()));
  });

  it("exits Add phone recovery without cancelling its invitation or applying late OAuth", async () => {
    const signIn = deferred<HostedAccountView>();
    const onExit = vi.fn();
    vi.spyOn(bridge, "enrollment_status").mockResolvedValue(view({ mode: "hosted", stage: "error", errorCode: "hosted-session-required" }));
    vi.spyOn(bridge, "hosted_sign_in").mockReturnValue(signIn.promise);
    renderLanding({ addingPhone: true, enrolledWithoutPhone: true, onExit });

    fireEvent.click(await screen.findByRole("button", { name: "Sign in again with Google" }));
    fireEvent.click(screen.getByRole("button", { name: "Close" }));
    await act(async () => signIn.resolve(account({ signedIn: true, hasVault: true, access: "read_write" })));

    expect(onExit).toHaveBeenCalledOnce();
    expect(bridge.enrollment_cancel).not.toHaveBeenCalled();
    expect(bridge.enrollment_status).toHaveBeenCalledOnce();
  });

  it("ignores a late recovered account after mode change or unmount", async () => {
    const signIn = deferred<HostedAccountView>();
    vi.spyOn(bridge, "hosted_account").mockResolvedValue(account({ signedIn: true, hasVault: true, access: "read_write" }));
    vi.spyOn(bridge, "enrollment_discover").mockResolvedValue(view({ mode: "hosted", stage: "error", errorCode: "hosted-session-required" }));
    vi.spyOn(bridge, "hosted_sign_in").mockReturnValue(signIn.promise);
    const rendered = renderLanding();

    fireEvent.click(await screen.findByRole("button", { name: "Sign in again with Google" }));
    rendered.rerender(<SetupLanding mode="self-hosted" onMode={vi.fn()} enrolledWithoutPhone={false} onJoined={vi.fn()} />);
    await act(async () => signIn.resolve(account({ signedIn: true, hasVault: true, access: "read_write" })));
    expect(bridge.enrollment_status).not.toHaveBeenCalled();

    const anotherSignIn = deferred<HostedAccountView>();
    vi.spyOn(bridge, "hosted_sign_in").mockReturnValue(anotherSignIn.promise);
    cleanup();
    const second = renderLanding();
    fireEvent.click(await screen.findByRole("button", { name: "Sign in again with Google" }));
    second.unmount();
    await act(async () => anotherSignIn.resolve(account({ signedIn: true, hasVault: true, access: "read_write" })));
    expect(bridge.enrollment_status).not.toHaveBeenCalled();
  });

  it("reconciles a recovered enrollment server state through hosted discovery", async () => {
    vi.spyOn(bridge, "hosted_account").mockResolvedValue(account({ signedIn: true, hasVault: true, access: "read_write" }));
    vi.spyOn(bridge, "enrollment_discover")
      .mockResolvedValueOnce(view({ mode: "hosted", stage: "error", errorCode: "hosted-session-required" }))
      .mockResolvedValueOnce(view({ mode: "hosted", stage: "waiting_phone", qrPayload: "fresh-qr" }));
    vi.spyOn(bridge, "hosted_sign_in").mockResolvedValue(account({ signedIn: true, hasVault: true, access: "read_write" }));
    vi.spyOn(bridge, "enrollment_status").mockResolvedValue(view({ mode: "hosted", stage: "server" }));
    renderLanding();

    fireEvent.click(await screen.findByRole("button", { name: "Sign in again with Google" }));

    expect(await screen.findByRole("img", { name: /pairing qr/i })).toBeInTheDocument();
    expect(bridge.enrollment_discover).toHaveBeenCalledTimes(2);
    expect(bridge.enrollment_discover).toHaveBeenLastCalledWith("https://peppy.pro");
  });

  it("returns a failed automatic phone invitation to manual setup after reauthentication", async () => {
    vi.spyOn(bridge, "enrollment_status")
      .mockResolvedValueOnce(view({ mode: "hosted", stage: "ready", owner: true, hasGateway: false }))
      .mockResolvedValueOnce(view({ mode: "hosted", stage: "ready", owner: true, hasGateway: false }));
    vi.spyOn(bridge, "enrollment_add_phone").mockRejectedValueOnce({ code: "hosted-session-required" }).mockResolvedValueOnce(waitingView({ mode: "hosted" }));
    vi.spyOn(bridge, "hosted_sign_in").mockResolvedValue(account({ signedIn: true, hasVault: true, access: "read_write" }));
    renderLanding({ enrolledWithoutPhone: true });

    fireEvent.click(await screen.findByRole("button", { name: "Sign in again with Google" }));
    const showCode = await screen.findByRole("button", { name: "Show connection code" });
    expect(bridge.enrollment_add_phone).toHaveBeenCalledOnce();
    fireEvent.click(showCode);
    await waitFor(() => expect(bridge.enrollment_add_phone).toHaveBeenCalledTimes(2));
  });

  it("routes a recovered resumable account through lapsed billing without another provision", async () => {
    vi.spyOn(bridge, "hosted_account")
      .mockResolvedValueOnce(account({ signedIn: true, access: "read_write", resumable: true }))
      .mockResolvedValue(account());
    vi.spyOn(bridge, "hosted_provision").mockRejectedValueOnce({ code: "hosted-session-required" });
    vi.spyOn(bridge, "hosted_sign_in").mockResolvedValue(account({ signedIn: true, resumable: true, classification: "lapsed", access: "read_only" }));
    renderLanding();

    fireEvent.click(await screen.findByRole("button", { name: "Sign in again with Google" }));
    expect(await screen.findByRole("button", { name: /check subscription/i })).toBeInTheDocument();
    expect(bridge.hosted_provision).toHaveBeenCalledOnce();
  });

  it("routes a recovered account without a vault to billing before provisioning", async () => {
    vi.spyOn(bridge, "hosted_account")
      .mockResolvedValueOnce(account({ signedIn: true, access: "read_write", resumable: true }))
      .mockResolvedValue(account());
    vi.spyOn(bridge, "hosted_provision").mockRejectedValueOnce({ code: "hosted-session-required" });
    vi.spyOn(bridge, "hosted_sign_in").mockResolvedValue(account({ signedIn: true, resumable: true, access: "read_only" }));
    renderLanding();

    fireEvent.click(await screen.findByRole("button", { name: "Sign in again with Google" }));
    expect(await screen.findByRole("button", { name: /check subscription/i })).toBeInTheDocument();
    expect(bridge.hosted_provision).toHaveBeenCalledOnce();
  });

  it("moves focus to recovery after a same-route fresh provisioning failure", async () => {
    vi.spyOn(bridge, "hosted_account").mockResolvedValue(account({ signedIn: true, access: "read_write" }));
    vi.spyOn(bridge, "hosted_provision").mockRejectedValue({ code: "hosted-session-required" });
    renderLanding();

    const create = await screen.findByRole("button", { name: "Create my vault" });
    create.focus();
    fireEvent.click(create);
    await waitFor(() => expect(screen.getByRole("heading", { name: "Sign in again to continue" })).toHaveFocus());
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

  it("keeps the new-user sign-in panel with an inline failure when Google returns signed out", async () => {
    vi.spyOn(bridge, "hosted_sign_in").mockResolvedValueOnce(account()).mockResolvedValueOnce(account({ signedIn: true, hasVault: true, access: "read_write" }));
    renderLanding();
    fireEvent.click(await screen.findByRole("button", { name: /i'm new to peppy/i }));
    const panel = document.getElementById("hosted-signin-panel")!;
    fireEvent.click(await within(panel).findByRole("button", { name: /continue with google/i }));

    const error = await within(panel).findByRole("alert");
    expect(error).toHaveAttribute("id", "hosted-signin-error");
    expect(error).toHaveTextContent(peppyCopy.production_signin_failed);
    expect(document.getElementById("hosted-session-recovery-panel")).not.toBeInTheDocument();
    const google = within(panel).getByRole("button", { name: /continue with google/i });
    expect(google).toBeEnabled();
    expect(bridge.hosted_provision).not.toHaveBeenCalled();
    expect(bridge.enrollment_status).not.toHaveBeenCalled();
    expect(bridge.enrollment_discover).not.toHaveBeenCalled();

    fireEvent.click(google);
    await waitFor(() => expect(bridge.enrollment_discover).toHaveBeenCalledWith("https://peppy.pro"));
    expect(bridge.hosted_sign_in).toHaveBeenCalledTimes(2);
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
    it("opens the invite request form from a claimed-server chooser", async () => {
      vi.spyOn(bridge, "enrollment_discover").mockResolvedValue(view({ stage: "signin" }));
      renderLanding({ fixedOrigin: "https://community.example" });

      fireEvent.click(await screen.findByRole("button", { name: /have an invite link/i }));

      expect(screen.getByLabelText(peppyCopy.enrollment_invite_link_label)).toBeInTheDocument();
      expect(screen.getByLabelText(peppyCopy.enrollment_display_name_label)).toBeInTheDocument();
    });

    it("keeps an invalid browser invite error inline with its form", async () => {
      vi.spyOn(bridge, "enrollment_discover").mockResolvedValue(view({ stage: "signin" }));
      vi.spyOn(bridge, "enrollment_begin_vault_request").mockRejectedValue({ code: "invite_invalid" });
      renderLanding({ fixedOrigin: "https://community.example" });

      fireEvent.click(await screen.findByRole("button", { name: /have an invite link/i }));
      fireEvent.change(screen.getByLabelText(peppyCopy.enrollment_invite_link_label), { target: { value: "https://community.example/#invite=invalid" } });
      fireEvent.change(screen.getByLabelText(peppyCopy.enrollment_display_name_label), { target: { value: "Avery" } });
      fireEvent.click(screen.getByRole("button", { name: peppyCopy.enrollment_invite_request_cta }));

      expect(await screen.findByText(peppyCopy.enrollment_error_invite_invalid)).toBeVisible();
      expect(screen.getByLabelText(peppyCopy.enrollment_invite_link_label)).toHaveAttribute("aria-invalid", "true");
    });

    it("offers a terminal restart rather than generic retry for a completed request without material", async () => {
      vi.spyOn(bridge, "enrollment_discover").mockResolvedValue(view({ stage: "error", errorCode: "request_completed_without_material" }));
      renderLanding({ fixedOrigin: "https://community.example" });

      expect(await screen.findByRole("heading", { name: "Vault request completed" })).toBeVisible();
      expect(screen.getByRole("button", { name: peppyCopy.enrollment_invite_restart_cta })).toBeVisible();
      expect(screen.queryByRole("button", { name: "Retry" })).toBeNull();
    });
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

    it("keeps explicitly open setup in open mode", async () => {
      vi.spyOn(bridge, "enrollment_discover").mockResolvedValue(view({ stage: "setup", setupCodeRequired: false }));
      renderLanding({ mode: "self-hosted" });
      fireEvent.change(screen.getByLabelText("Server URL"), { target: { value: "https://server.example" } });
      fireEvent.click(screen.getByRole("button", { name: "Connect" }));

      await screen.findByRole("heading", { name: "Set up your server" });
      const setup = document.getElementById("enrollment-setup")!;
      expect(setup).toHaveAttribute("data-setup-mode", "open");
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

    it("shows the admin password length message and code without rendering a native password input", async () => {
      vi.spyOn(bridge, "enrollment_discover").mockResolvedValue(view({ stage: "error", errorCode: "admin-password-length" }));
      renderLanding({ fixedOrigin: "https://community.example" });

      expect(await screen.findByRole("alert")).toHaveTextContent("Admin password must be 12–1024 bytes long (usually one byte per character).");
      expect(document.getElementById("enrollment-error-code")).toHaveAttribute("data-error-code", "admin-password-length");
      expect(document.querySelector("input[type=password]")).not.toBeInTheDocument();
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
      fireEvent.click(await screen.findByRole("button", { name: /sign in with your username/i }));
      fireEvent.click(await screen.findByRole("button", { name: "Sign in" }));
      await waitFor(() => expect(bridge.enrollment_sign_in).toHaveBeenCalledOnce());
      expect(document.body).toHaveTextContent(/not your account or admin password/i);
    });

    it("signs in to a hosted stage through the enrollment bridge exactly once", async () => {
      vi.spyOn(bridge, "enrollment_discover").mockResolvedValue(view({ stage: "signin", mode: "hosted" }));
      vi.spyOn(bridge, "enrollment_sign_in").mockResolvedValue(view({ stage: "locked", mode: "hosted" }));
      renderLanding();
      fireEvent.click(await screen.findByRole("button", { name: /already use peppy/i }));
      fireEvent.click(await screen.findByRole("button", { name: "Sign in" }));
      expect(await screen.findByRole("button", { name: "Enter Vault key phrase" })).toBeInTheDocument();
      expect(bridge.enrollment_sign_in).toHaveBeenCalledOnce();
      expect(bridge.hosted_sign_in).not.toHaveBeenCalled();
      expect(bridge.enrollment_discover).toHaveBeenCalledTimes(1);
    });

    it("uses the community sign-in bridge for a fixed origin even when the outer mode is hosted", async () => {
      vi.spyOn(bridge, "enrollment_discover").mockResolvedValue(view({ stage: "signin", mode: "community", origin: "https://community.example" }));
      renderLanding({ fixedOrigin: "https://community.example" });
      expect(await screen.findByRole("heading", { name: "Sign in to your server" })).toBeInTheDocument();
      expect(document.body).not.toHaveTextContent(/peppy account|hosted/i);
      fireEvent.click(screen.getByRole("button", { name: /sign in with your username/i }));
      fireEvent.click(await screen.findByRole("button", { name: "Sign in" }));
      await waitFor(() => expect(bridge.enrollment_sign_in).toHaveBeenCalledOnce());
      expect(bridge.hosted_sign_in).not.toHaveBeenCalled();
    });

    it("shows the vault-owner field when sign-in requires an owner selection", async () => {
      vi.spyOn(bridge, "enrollment_discover").mockResolvedValue(view({ stage: "signin", mode: "community" }));
      vi.spyOn(bridge, "enrollment_sign_in").mockResolvedValue(view({ stage: "locked", vaultOwnerRequired: true }));
      renderLanding({ fixedOrigin: "https://community.example" });

      fireEvent.click(await screen.findByRole("button", { name: /sign in with your username/i }));
      fireEvent.click(await screen.findByRole("button", { name: "Sign in" }));

      expect(await screen.findByLabelText(peppyCopy.enrollment_vault_owner_label)).toBeVisible();
      expect(screen.getByRole("button", { name: "Enter Vault key phrase" })).toBeVisible();
    });

    it("lets a community user connect with the shared secret without an admin sign-in", async () => {
      vi.spyOn(bridge, "enrollment_discover").mockResolvedValue(view({ stage: "signin", mode: "community", origin: "https://community.example" }));
      renderLanding({ fixedOrigin: "https://community.example" });
      fireEvent.click(await screen.findByRole("button", { name: /connect this device/i }));
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
      fireEvent.click(await screen.findByRole("button", { name: /connect this device/i }));
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
      fireEvent.click(await screen.findByRole("button", { name: "Enter Vault key phrase" }));
      await waitFor(() => expect(bridge.enrollment_connect_shared_secret).toHaveBeenCalledOnce());
      expect(document.querySelector("input[type=password]")).not.toBeInTheDocument();
    });

    it("rotates to the shared-secret phone method", async () => {
      vi.spyOn(bridge, "enrollment_discover").mockResolvedValue(view({ stage: "ready", owner: true, hasGateway: false }));
      renderLanding({ mode: "self-hosted" });
      fireEvent.change(screen.getByLabelText("Server URL"), { target: { value: "https://server.example" } });
      fireEvent.click(screen.getByRole("button", { name: "Connect" }));
      fireEvent.click(await screen.findByRole("button", { name: "Enter Vault key phrase on phone" }));
      await waitFor(() => expect(bridge.enrollment_rotate_phone).toHaveBeenCalledWith("shared_secret"));
      expect(bridge.reveal_vault_secret).not.toHaveBeenCalled();
      Object.defineProperty(window, "__TAURI_INTERNALS__", { configurable: true, value: {} });
      fireEvent.click(screen.getByRole("button", { name: "Enter Vault key phrase on phone" }));
      await waitFor(() => expect(bridge.reveal_vault_secret).toHaveBeenCalledOnce());
      delete (window as Window & { __TAURI_INTERNALS__?: unknown }).__TAURI_INTERNALS__;
    });

    it("shows an inline secret-reveal failure after rotating the phone method", async () => {
      vi.spyOn(bridge, "enrollment_discover").mockResolvedValue(waitingView());
      vi.spyOn(bridge, "reveal_vault_secret").mockRejectedValue({ code: "vault-secret-unavailable" });
      Object.defineProperty(window, "__TAURI_INTERNALS__", { configurable: true, value: {} });
      renderLanding({ mode: "self-hosted" });
      fireEvent.change(screen.getByLabelText("Server URL"), { target: { value: "https://server.example" } });
      fireEvent.click(screen.getByRole("button", { name: "Connect" }));
      fireEvent.click(await screen.findByRole("button", { name: "Enter Vault key phrase on phone" }));

      expect(await screen.findByRole("alert")).toHaveAttribute("id", "enrollment-shared-secret-error");
      delete (window as Window & { __TAURI_INTERNALS__?: unknown }).__TAURI_INTERNALS__;
    });

    it("shows a waiting message instead of an owner action to a non-owner device", async () => {
      vi.spyOn(bridge, "enrollment_status").mockResolvedValue(view({ stage: "ready", owner: false }));
      renderLanding({ enrolledWithoutPhone: true });
      expect(await screen.findByRole("heading", { name: /waiting for an owner/i })).toBeInTheDocument();
      expect(screen.queryByRole("button", { name: "Add phone" })).not.toBeInTheDocument();
    });

    it("keeps owner actions available when automatic invitation creation returns ready", async () => {
      vi.spyOn(bridge, "enrollment_discover").mockResolvedValue(view({ stage: "ready", owner: true, hasGateway: false }));
      vi.spyOn(bridge, "enrollment_add_phone").mockResolvedValue(view({ stage: "ready", owner: true, hasGateway: false }));
      renderLanding({ mode: "self-hosted" });
      fireEvent.change(screen.getByLabelText("Server URL"), { target: { value: "https://server.example" } });
      fireEvent.click(screen.getByRole("button", { name: "Connect" }));

      expect(await screen.findByRole("button", { name: "Show connection code" })).toBeEnabled();
      expect(screen.getByRole("button", { name: "Set up phone later" })).toBeEnabled();
    });

    it("hides ready recovery actions while the host connection is pending", async () => {
      const invitation = deferred<EnrollmentView>();
      vi.spyOn(bridge, "enrollment_discover").mockResolvedValue(view({ stage: "ready", owner: true, hasGateway: false }));
      vi.spyOn(bridge, "enrollment_add_phone").mockReturnValue(invitation.promise);
      const onMode = vi.fn();
      const onJoined = vi.fn();
      const { rerender } = render(
        <SetupLanding
          mode="self-hosted"
          onMode={onMode}
          enrolledWithoutPhone={false}
          onJoined={onJoined}
          fixedOrigin="https://community.example"
          connectionPending
        />,
      );

      await waitFor(() => expect(bridge.enrollment_add_phone).toHaveBeenCalledWith("transfer"));
      await act(async () => invitation.resolve(view({ stage: "ready", owner: true, hasGateway: false })));
      expect(screen.queryByRole("button", { name: "Show connection code" })).not.toBeInTheDocument();
      expect(screen.queryByRole("button", { name: "Set up phone later" })).not.toBeInTheDocument();

      rerender(
        <SetupLanding
          mode="self-hosted"
          onMode={onMode}
          enrolledWithoutPhone={false}
          onJoined={onJoined}
          fixedOrigin="https://community.example"
        />,
      );

      expect(await screen.findByRole("button", { name: "Show connection code" })).toBeEnabled();
      expect(screen.getByRole("button", { name: "Set up phone later" })).toBeEnabled();
    });

    it("hides the unknown-roster defer action while the host connection is pending", async () => {
      vi.spyOn(bridge, "enrollment_status").mockResolvedValue(view({ stage: "ready", owner: true }));
      const onMode = vi.fn();
      const onJoined = vi.fn();
      const onDeferPhone = vi.fn();
      const { rerender } = render(
        <SetupLanding
          mode="self-hosted"
          onMode={onMode}
          enrolledWithoutPhone
          onJoined={onJoined}
          onDeferPhone={onDeferPhone}
          connectionPending
        />,
      );

      await screen.findByRole("heading", { name: "Connect your phone" });
      expect(screen.queryByRole("button", { name: "Set up phone later" })).not.toBeInTheDocument();

      rerender(
        <SetupLanding
          mode="self-hosted"
          onMode={onMode}
          enrolledWithoutPhone
          onJoined={onJoined}
          onDeferPhone={onDeferPhone}
        />,
      );

      expect(await screen.findByRole("button", { name: "Set up phone later" })).toBeEnabled();
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

    it("defers phone setup only after cancelling the active phone invitation", async () => {
      const onDeferPhone = vi.fn();
      renderLanding({ mode: "self-hosted", onDeferPhone });
      fireEvent.change(screen.getByLabelText("Server URL"), { target: { value: "https://server.example" } });
      fireEvent.click(screen.getByRole("button", { name: "Connect" }));
      fireEvent.click(await screen.findByRole("button", { name: "Set up phone later" }));
      await waitFor(() => expect(bridge.enrollment_cancel_phone).toHaveBeenCalledOnce());
      await waitFor(() => expect(onDeferPhone).toHaveBeenCalledOnce());
    });

    it("keeps the verification step when a phone claimed the code before Set up phone later", async () => {
      const onDeferPhone = vi.fn();
      vi.spyOn(bridge, "enrollment_cancel_phone").mockResolvedValue(view({ stage: "verify_phone", owner: true, verificationCode: "123456" }));
      renderLanding({ mode: "self-hosted", onDeferPhone });
      fireEvent.change(screen.getByLabelText("Server URL"), { target: { value: "https://server.example" } });
      fireEvent.click(screen.getByRole("button", { name: "Connect" }));
      fireEvent.click(await screen.findByRole("button", { name: "Set up phone later" }));
      await waitFor(() => expect(bridge.enrollment_cancel_phone).toHaveBeenCalledOnce());
      expect(await screen.findByRole("heading", { name: /verify the code/i })).toBeInTheDocument();
      expect(onDeferPhone).not.toHaveBeenCalled();
    });

    it("starts a connection code when Add phone is used by an owner who already has a phone", async () => {
      vi.spyOn(bridge, "enrollment_status").mockResolvedValue(view({ stage: "ready", owner: true, hasGateway: true }));
      renderLanding({ mode: "self-hosted", addingPhone: true, onExit: vi.fn() });
      await waitFor(() => expect(bridge.enrollment_add_phone).toHaveBeenCalledWith("transfer"));
      expect(await screen.findByRole("img", { name: /pairing qr/i })).toBeInTheDocument();
    });

    it("offers to finish an earlier setup from sign-in on a claimed server", async () => {
      vi.spyOn(bridge, "enrollment_discover").mockResolvedValue(view({ stage: "signin", ownerResumePending: true }));
      renderLanding({ mode: "self-hosted" });
      fireEvent.change(screen.getByLabelText("Server URL"), { target: { value: "https://server.example" } });
      fireEvent.click(screen.getByRole("button", { name: "Connect" }));
      fireEvent.click(await screen.findByRole("button", { name: new RegExp(peppyCopy.setup_owner_resume_signin_label) }));
      await waitFor(() => expect(bridge.enrollment_create_owner).toHaveBeenCalledOnce());
    });

    it("hides the earlier-setup option on sign-in when no attempt is pending", async () => {
      vi.spyOn(bridge, "enrollment_discover").mockResolvedValue(view({ stage: "signin" }));
      renderLanding({ mode: "self-hosted" });
      fireEvent.change(screen.getByLabelText("Server URL"), { target: { value: "https://server.example" } });
      fireEvent.click(screen.getByRole("button", { name: "Connect" }));
      await screen.findByRole("heading", { name: "Sign in to your server" });
      expect(screen.queryByRole("button", { name: new RegExp(peppyCopy.setup_owner_resume_signin_label) })).not.toBeInTheDocument();
    });

    it("rotates once near expiry but never rotates during verification", async () => {
      const advance = capturePoll();
      vi.spyOn(bridge, "enrollment_discover").mockResolvedValue(waitingView({ expiresInSeconds: 15 }));
      vi.spyOn(bridge, "enrollment_status").mockResolvedValue(waitingView({ expiresInSeconds: 10 }));
      renderLanding({ mode: "self-hosted" });
      fireEvent.change(screen.getByLabelText("Server URL"), { target: { value: "https://server.example" } });
      fireEvent.click(screen.getByRole("button", { name: "Connect" }));
      await waitFor(() => expect(bridge.enrollment_rotate_phone).toHaveBeenCalledWith("transfer"));
      await advance();
      expect(bridge.enrollment_rotate_phone).toHaveBeenCalledOnce();
      cleanup();
      vi.clearAllMocks();
      vi.spyOn(bridge, "enrollment_discover").mockResolvedValue(view({ stage: "verify_phone", verificationCode: "123" }));
      renderLanding({ mode: "self-hosted" });
      fireEvent.change(screen.getByLabelText("Server URL"), { target: { value: "https://server.example" } });
      fireEvent.click(screen.getByRole("button", { name: "Connect" }));
      await screen.findByLabelText("Verification code: 123");
      expect(bridge.enrollment_rotate_phone).not.toHaveBeenCalled();
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

    it("retries a returned polling error with status instead of discovery", async () => {
      const advance = capturePoll();
      vi.spyOn(bridge, "enrollment_status")
        .mockResolvedValueOnce(view({ stage: "error", errorCode: "offline" }))
        .mockResolvedValueOnce(waitingView());
      renderLanding({ mode: "self-hosted" });
      fireEvent.change(screen.getByLabelText("Server URL"), { target: { value: "https://server.example" } });
      fireEvent.click(screen.getByRole("button", { name: "Connect" }));
      await screen.findByRole("img", { name: /pairing qr/i });

      await advance();
      fireEvent.click(await screen.findByRole("button", { name: "Retry" }));

      await waitFor(() => expect(bridge.enrollment_status).toHaveBeenCalledTimes(2));
      expect(await screen.findByRole("img", { name: /pairing qr/i })).toBeInTheDocument();
      expect(bridge.enrollment_discover).toHaveBeenCalledOnce();
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

    it("keeps manual restart after a rotation returns expired", async () => {
      vi.spyOn(bridge, "enrollment_discover").mockResolvedValue(view({ stage: "expired", owner: true }));
      vi.spyOn(bridge, "enrollment_rotate_phone").mockResolvedValue(view({ stage: "expired", owner: true }));
      renderLanding({ mode: "self-hosted" });
      fireEvent.change(screen.getByLabelText("Server URL"), { target: { value: "https://server.example" } });
      fireEvent.click(screen.getByRole("button", { name: "Connect" }));
      await waitFor(() => expect(bridge.enrollment_rotate_phone).toHaveBeenCalledWith("transfer"));
      expect(await screen.findByRole("button", { name: "Start again" })).toBeEnabled();
      expect(bridge.enrollment_rotate_phone).toHaveBeenCalledOnce();
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

    it("ignores a poll that was in flight when the user cancels", async () => {
      const advance = capturePoll();
      const stalePoll = deferred<EnrollmentView>();
      vi.spyOn(bridge, "enrollment_status").mockReturnValueOnce(stalePoll.promise).mockResolvedValueOnce(view({ stage: "ready", owner: true }));
      renderLanding({ mode: "self-hosted" });
      fireEvent.change(screen.getByLabelText("Server URL"), { target: { value: "https://server.example" } });
      fireEvent.click(screen.getByRole("button", { name: "Connect" }));
      await screen.findByRole("img", { name: /pairing qr/i });
      await advance();
      fireEvent.click(screen.getByRole("button", { name: "Cancel" }));
      await waitFor(() => expect(bridge.enrollment_cancel).toHaveBeenCalledOnce());
      await waitFor(() => expect(bridge.enrollment_status).toHaveBeenCalledTimes(2));
      await act(async () => stalePoll.resolve(waitingView()));
      expect(document.getElementById("enrollment-qr-image")).not.toBeInTheDocument();
    });
  });

  describe("native connection failure codes", () => {
    async function failedDiscoveryAlert(rejection: unknown) {
      vi.spyOn(bridge, "enrollment_discover").mockRejectedValueOnce(rejection);
      renderLanding({ mode: "self-hosted" });
      fireEvent.change(screen.getByLabelText("Server URL"), { target: { value: "https://server.example" } });
      fireEvent.click(screen.getByRole("button", { name: "Connect" }));
      return screen.findByRole("alert");
    }

    it.each([
      ["offline", "offline"],
      ["network", "network"],
      ["local TLS", "local-tls-config"],
      ["secure store", "secure-store-unavailable"],
      ["server rejected", "server-rejected"],
      ["response too large", "response-too-large"],
      ["host state", "host-state"],
      ["invalid origin", "invalid-origin"],
      ["server selection", "server-required"],
      ["enrollment", "enrollment"],
      ["saved credential missing", "credentials-required"],
      ["local database key missing", "database-key-missing"],
      ["local database key mismatch", "database-key-mismatch"],
      ["local identity mismatch", "credential-mismatch"],
      ["saved credential origin mismatch", "origin-binding"],
      ["native host configuration", "host-config"],
      ["server response", "server-response"],
      ["revoked device credential", "revoked"],
      ["recovery secret mismatch", "wrong-passphrase"],
      ["credential changed during setup", "credential-changed"],
      ["phone not finished", "enrollment-state"],
      ["database key conflict", "database-key-conflict"],
    ])("reports the typed %s failure as its own code", async (_label, code) => {
      const generic = await failedDiscoveryAlert({ code: "not-a-native-code", message: "ignored" });
      const genericText = generic.textContent;
      cleanup();

      const alert = await failedDiscoveryAlert({ code, message: "native detail that must stay hidden" });

      expect(document.getElementById("enrollment-error-code")).toHaveAttribute("data-error-code", code);
      expect(alert.textContent).not.toBe(genericText);
      expect(document.body).not.toHaveTextContent("native detail that must stay hidden");
    });

    it("keeps a known setup-token domain error on its domain message", async () => {
      vi.spyOn(bridge, "enrollment_discover").mockResolvedValue(view({ stage: "setup", setupCodeRequired: true }));
      vi.spyOn(bridge, "enrollment_create_owner").mockRejectedValueOnce({ code: "invalid_setup_token", message: "raw" });
      renderLanding({ mode: "self-hosted" });
      fireEvent.change(screen.getByLabelText("Server URL"), { target: { value: "https://server.example" } });
      fireEvent.click(screen.getByRole("button", { name: "Connect" }));
      fireEvent.click(await screen.findByRole("button", { name: "Create server" }));

      expect(await screen.findByRole("alert")).toHaveTextContent(peppyCopy.enrollment_error_invalid_setup_token);
      expect(document.getElementById("enrollment-error-code")).toHaveAttribute("data-error-code", "invalid_setup_token");
    });

    it("directs a missing vault secret to the native Settings unlock before retrying", async () => {
      await failedDiscoveryAlert({ code: "vault-secret-unavailable", message: "raw secret detail" });

      const alert = document.getElementById("enrollment-error")!;
      expect(alert).toHaveTextContent(/Settings/);
      expect(alert).toHaveTextContent(/Sync encryption/);
      expect(alert).toHaveTextContent(/Unlock sync natively/);
      expect(alert).toHaveTextContent(/retry/i);
      expect(document.getElementById("enrollment-error-code")).toHaveAttribute("data-error-code", "vault-secret-unavailable");
      expect(within(document.getElementById("enrollment-panel")!).getByRole("button", { name: "Retry" })).toBeInTheDocument();
      expect(document.body).not.toHaveTextContent("raw secret detail");
    });

    it.each([
      "credentials-required",
      "database-key-missing",
      "database-key-mismatch",
      "credential-mismatch",
      "origin-binding",
      "secure-store-unavailable",
      "database-key-conflict",
      "host-config",
    ])("offers only a retry for the local storage failure %s and never a reset or removal action", async code => {
      await failedDiscoveryAlert({ code, message: "raw storage detail" });

      const panel = document.getElementById("enrollment-panel")!;
      expect(within(panel).getAllByRole("button").map(button => button.textContent)).toEqual(["Retry"]);
      expect(panel.textContent).not.toMatch(/delete|remove|erase|wipe|clear|reset/i);
      expect(document.body).not.toHaveTextContent("raw storage detail");
    });

    it.each(["vault_not_found", "request_not_found", "request_not_approved", "operation_conflict"])("keeps the recognized claim code %s visible", async code => {
      await failedDiscoveryAlert({ code, message: "raw claim detail" });

      expect(document.getElementById("enrollment-error-code")).toHaveAttribute("data-error-code", code);
      expect(document.body).not.toHaveTextContent("raw claim detail");
    });

    it("reads the code from an Error instance and hides its secret-bearing message", async () => {
      await failedDiscoveryAlert(Object.assign(new Error("token=abc at https://leak.example /Users/someone"), { code: "host-config" }));

      expect(document.getElementById("enrollment-error-code")).toHaveAttribute("data-error-code", "host-config");
      expect(document.body).not.toHaveTextContent("token=abc");
      expect(document.body).not.toHaveTextContent("leak.example");
      expect(document.body).not.toHaveTextContent("/Users/someone");
    });

    it("treats a thrown string as unavailable and never renders it", async () => {
      await failedDiscoveryAlert("token=abc at https://leak.example");

      expect(document.getElementById("enrollment-error-code")).toHaveAttribute("data-error-code", "unavailable");
      expect(document.body).not.toHaveTextContent("token=abc");
      expect(document.body).not.toHaveTextContent("leak.example");
    });

    it("keeps the code on a failed phone status poll without changing the polling stage", async () => {
      const advance = capturePoll();
      vi.spyOn(bridge, "enrollment_status").mockRejectedValueOnce({ code: "revoked", message: "token=abc" });
      renderLanding({ mode: "self-hosted" });
      fireEvent.change(screen.getByLabelText("Server URL"), { target: { value: "https://server.example" } });
      fireEvent.click(screen.getByRole("button", { name: "Connect" }));
      await screen.findByRole("img", { name: /pairing qr/i });

      await advance();

      expect(await screen.findByRole("alert")).toBeInTheDocument();
      expect(document.getElementById("enrollment-error-code")).toHaveAttribute("data-error-code", "revoked");
      expect(document.body).not.toHaveTextContent("token=abc");

      fireEvent.click(screen.getByRole("button", { name: "Retry" }));
      await waitFor(() => expect(bridge.enrollment_status).toHaveBeenCalledTimes(2));
      expect(bridge.enrollment_discover).toHaveBeenCalledOnce();
    });

    it("hides unknown codes, URLs, and raw messages behind the unavailable code", async () => {
      await failedDiscoveryAlert({ code: "https://leak.example/token=abc", message: "token=abc at /Users/someone/secret" });

      expect(document.getElementById("enrollment-error-code")).toHaveAttribute("data-error-code", "unavailable");
      expect(document.body).not.toHaveTextContent("leak.example");
      expect(document.body).not.toHaveTextContent("token=abc");
      expect(document.body).not.toHaveTextContent("/Users/someone/secret");
    });

    it("retries the same typed server origin after a typed failure", async () => {
      vi.spyOn(bridge, "enrollment_discover").mockRejectedValueOnce({ code: "offline", message: "x" }).mockResolvedValueOnce(waitingView());
      renderLanding({ mode: "self-hosted" });
      fireEvent.change(screen.getByLabelText("Server URL"), { target: { value: "https://server.example" } });
      fireEvent.click(screen.getByRole("button", { name: "Connect" }));
      await screen.findByRole("alert");
      expect(screen.getByLabelText("Server URL")).toHaveValue("https://server.example");

      fireEvent.click(screen.getByRole("button", { name: "Retry" }));

      expect(await screen.findByRole("img", { name: /pairing qr/i })).toBeInTheDocument();
      expect(bridge.enrollment_discover).toHaveBeenNthCalledWith(1, "https://server.example");
      expect(bridge.enrollment_discover).toHaveBeenNthCalledWith(2, "https://server.example");
    });

    it("ignores a typed failure from an attempt that was already superseded", async () => {
      const discovery = deferred<EnrollmentView>();
      vi.spyOn(bridge, "enrollment_discover").mockReturnValue(discovery.promise);
      renderLanding();
      await chooseExisting();
      fireEvent.click(await screen.findByRole("button", { name: "Back" }));

      await act(async () => discovery.reject({ code: "secure-store-unavailable", message: "stale" }));

      expect(document.getElementById("enrollment-error")).not.toBeInTheDocument();
      expect(document.getElementById("enrollment-error-code")).not.toBeInTheDocument();
      expect(screen.getByRole("button", { name: /already use peppy/i })).toBeInTheDocument();
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

  describe("async pairing operations", () => {
    const pendingIndicator = () => document.getElementById("setup-pending-operation");
    const qrImage = () => document.getElementById("enrollment-qr-image");
    const markNative = () => Object.defineProperty(window, "__TAURI_INTERNALS__", { configurable: true, value: {} });

    afterEach(() => {
      delete (window as Window & { __TAURI_INTERNALS__?: unknown }).__TAURI_INTERNALS__;
    });

    it("shows one preparing status while automatic invitation creation is pending", async () => {
      const creation = deferred<EnrollmentView>();
      vi.spyOn(bridge, "enrollment_status").mockResolvedValue(view({ stage: "ready", owner: true, hasGateway: false }));
      vi.spyOn(bridge, "enrollment_add_phone").mockReturnValue(creation.promise);
      renderLanding({ enrolledWithoutPhone: true });

      await waitFor(() => expect(bridge.enrollment_add_phone).toHaveBeenCalledWith("transfer"));
      expect(screen.getAllByText(peppyCopy.production_pairing_preparing)).toHaveLength(1);
      await act(async () => creation.resolve(waitingView()));
    });

    it("does not implicitly cancel a claimed invitation on unmount", async () => {
      vi.spyOn(bridge, "enrollment_discover").mockResolvedValue(view({ stage: "verify_phone", owner: true, verificationCode: "123456" }));
      const rendered = renderLanding({ fixedOrigin: "https://community.example" });
      await screen.findByRole("heading", { name: /verify the code/i });

      rendered.unmount();

      expect(bridge.enrollment_cancel).not.toHaveBeenCalled();
    });

    it("allows explicit cancellation after a phone claims the invitation", async () => {
      vi.spyOn(bridge, "enrollment_discover").mockResolvedValue(view({ stage: "verify_phone", owner: true, verificationCode: "123456" }));
      renderLanding({ fixedOrigin: "https://community.example" });

      fireEvent.click(await screen.findByRole("button", { name: "Cancel" }));

      await waitFor(() => expect(bridge.enrollment_cancel).toHaveBeenCalledOnce());
    });

    it("shows encoding progress while the QR promise is pending and reveals the image once it resolves", async () => {
      const encoded = deferred<string>();
      vi.spyOn(QRCode, "toDataURL").mockImplementation(() => encoded.promise);
      vi.spyOn(bridge, "enrollment_discover").mockResolvedValue(waitingView());
      renderLanding({ fixedOrigin: "https://community.example" });

      expect(await screen.findByText(peppyCopy.production_pairing_qr_generating)).toBeInTheDocument();
      expect(qrImage()).not.toBeInTheDocument();
      await act(async () => encoded.resolve("data:image/png;base64,AAAA"));
      expect(await screen.findByRole("img", { name: /pairing qr/i })).toBeInTheDocument();
      expect(qrImage()).toHaveAttribute("src", "data:image/png;base64,AAAA");
      expect(screen.queryByText(peppyCopy.production_pairing_qr_generating)).not.toBeInTheDocument();
    });

    it("retries a rejected QR encoding with the same payload and creates no new invitation", async () => {
      const encode = vi.spyOn(QRCode, "toDataURL")
        .mockImplementationOnce(() => Promise.reject(new Error("encoder failed")))
        .mockImplementationOnce(() => Promise.resolve("data:image/png;base64,AAAA"));
      vi.spyOn(bridge, "enrollment_discover").mockResolvedValue(waitingView());
      renderLanding({ fixedOrigin: "https://community.example" });

      fireEvent.click(await screen.findByRole("button", { name: /try again/i }));

      expect(await screen.findByRole("img", { name: /pairing qr/i })).toBeInTheDocument();
      expect(encode.mock.calls.map(([payload]) => payload)).toEqual(["secret-qr-payload", "secret-qr-payload"]);
      expect(bridge.enrollment_discover).toHaveBeenCalledOnce();
      expect(bridge.enrollment_add_phone).not.toHaveBeenCalled();
      expect(bridge.enrollment_rotate_phone).not.toHaveBeenCalled();
    });

    it("ignores a delayed encoding for an earlier payload after a newer invitation is shown", async () => {
      const advance = capturePoll();
      const oldEncoding = deferred<string>();
      vi.spyOn(QRCode, "toDataURL").mockImplementation(((payload: string) => payload === "old-qr-payload"
        ? oldEncoding.promise
        : Promise.resolve(`data:${payload}`)) as typeof QRCode.toDataURL);
      vi.spyOn(bridge, "enrollment_discover").mockResolvedValue(waitingView({ qrPayload: "old-qr-payload" }));
      vi.spyOn(bridge, "enrollment_status").mockResolvedValue(waitingView({ qrPayload: "new-qr-payload" }));
      renderLanding({ fixedOrigin: "https://community.example" });
      await screen.findByText(peppyCopy.production_pairing_qr_generating);

      await advance();
      await waitFor(() => expect(qrImage()).toHaveAttribute("src", "data:new-qr-payload"));
      await act(async () => oldEncoding.resolve("data:old-qr-payload"));

      expect(qrImage()).toHaveAttribute("src", "data:new-qr-payload");
    });

    it("shows refreshing progress and blocks conflicting actions during automatic rotation", async () => {
      const rotation = deferred<EnrollmentView>();
      vi.spyOn(bridge, "enrollment_discover").mockResolvedValue(waitingView({ expiresInSeconds: 15 }));
      vi.spyOn(bridge, "enrollment_rotate_phone").mockReturnValue(rotation.promise);
      renderLanding({ fixedOrigin: "https://community.example" });

      expect(await screen.findByText(peppyCopy.production_pairing_refreshing)).toBeInTheDocument();
      expect(screen.getByRole("button", { name: "Cancel" })).toBeDisabled();
      expect(screen.getByRole("button", { name: "Enter Vault key phrase on phone" })).toBeDisabled();
      expect(screen.getByRole("button", { name: "Set up phone later" })).toBeDisabled();

      await act(async () => rotation.resolve(waitingView({ expiresInSeconds: 300 })));
      await waitFor(() => expect(pendingIndicator()).not.toBeInTheDocument());
      expect(screen.getByRole("button", { name: "Cancel" })).toBeEnabled();
      expect(bridge.enrollment_rotate_phone).toHaveBeenCalledOnce();
      expect(bridge.enrollment_rotate_phone).toHaveBeenCalledWith("transfer");
    });

    it("keeps background status polls quiet with no pending indicator or disabled actions", async () => {
      const advance = capturePoll();
      const poll = deferred<EnrollmentView>();
      vi.spyOn(bridge, "enrollment_discover").mockResolvedValue(waitingView());
      vi.spyOn(bridge, "enrollment_status").mockReturnValue(poll.promise);
      renderLanding({ fixedOrigin: "https://community.example" });
      await screen.findByRole("img", { name: /pairing qr/i });

      await advance();

      expect(bridge.enrollment_status).toHaveBeenCalledOnce();
      expect(pendingIndicator()).not.toBeInTheDocument();
      expect(screen.getByRole("button", { name: "Cancel" })).toBeEnabled();
      expect(screen.getByRole("button", { name: "Enter Vault key phrase on phone" })).toBeEnabled();
      await act(async () => poll.resolve(waitingView()));
    });

    it("keeps the shared-secret phone action busy until the native reveal settles", async () => {
      markNative();
      const reveal = deferred<void>();
      vi.spyOn(bridge, "enrollment_discover").mockResolvedValue(waitingView());
      vi.spyOn(bridge, "reveal_vault_secret").mockReturnValue(reveal.promise);
      renderLanding({ fixedOrigin: "https://community.example" });
      fireEvent.click(await screen.findByRole("button", { name: "Enter Vault key phrase on phone" }));

      await waitFor(() => expect(bridge.reveal_vault_secret).toHaveBeenCalledOnce());
      expect(bridge.enrollment_rotate_phone).toHaveBeenCalledWith("shared_secret");
      expect(pendingIndicator()).toHaveTextContent(peppyCopy.production_pairing_revealing);
      expect(screen.getByRole("button", { name: "Cancel" })).toBeDisabled();
      expect(screen.getByRole("button", { name: "Enter Vault key phrase on phone" })).toBeDisabled();

      await act(async () => reveal.resolve());
      await waitFor(() => expect(pendingIndicator()).not.toBeInTheDocument());
      expect(screen.getByRole("button", { name: "Cancel" })).toBeEnabled();
    });

    it("keeps the shared-secret method for near-expiry rotation and its hint after the reveal settles", async () => {
      markNative();
      const advance = capturePoll();
      vi.spyOn(bridge, "enrollment_discover").mockResolvedValue(waitingView());
      vi.spyOn(bridge, "enrollment_status").mockResolvedValue(waitingView({ expiresInSeconds: 10 }));
      renderLanding({ fixedOrigin: "https://community.example" });
      fireEvent.click(await screen.findByRole("button", { name: "Enter Vault key phrase on phone" }));
      await waitFor(() => expect(bridge.reveal_vault_secret).toHaveBeenCalledOnce());
      await waitFor(() => expect(pendingIndicator()).not.toBeInTheDocument());
      expect(screen.getByText("After scanning, your phone asks for the vault key phrase. Enter it there, then compare the codes.")).toBeInTheDocument();

      await advance();

      await waitFor(() => expect(bridge.enrollment_rotate_phone).toHaveBeenCalledTimes(2));
      expect(bridge.enrollment_rotate_phone).toHaveBeenNthCalledWith(1, "shared_secret");
      expect(bridge.enrollment_rotate_phone).toHaveBeenNthCalledWith(2, "shared_secret");
    });

    it.each(["verify_phone", "phone_approved", "expired"] as const)("never reveals the shared secret when rotation returns %s", async stage => {
      markNative();
      vi.spyOn(bridge, "enrollment_discover").mockResolvedValue(waitingView());
      vi.spyOn(bridge, "enrollment_rotate_phone").mockResolvedValue(view({ stage, owner: true, verificationCode: "123456" }));
      renderLanding({ fixedOrigin: "https://community.example" });
      fireEvent.click(await screen.findByRole("button", { name: "Enter Vault key phrase on phone" }));

      await waitFor(() => expect(bridge.enrollment_rotate_phone).toHaveBeenCalledWith("shared_secret"));
      await waitFor(() => expect(pendingIndicator()).not.toBeInTheDocument());
      expect(bridge.reveal_vault_secret).not.toHaveBeenCalled();
    });

    it("does not reveal the shared secret when the landing unmounts before rotation settles", async () => {
      markNative();
      const rotation = deferred<EnrollmentView>();
      vi.spyOn(bridge, "enrollment_discover").mockResolvedValue(waitingView());
      vi.spyOn(bridge, "enrollment_rotate_phone").mockReturnValue(rotation.promise);
      const rendered = renderLanding({ fixedOrigin: "https://community.example" });
      fireEvent.click(await screen.findByRole("button", { name: "Enter Vault key phrase on phone" }));
      await waitFor(() => expect(bridge.enrollment_rotate_phone).toHaveBeenCalledWith("shared_secret"));

      rendered.unmount();
      await act(async () => rotation.resolve(waitingView()));

      expect(bridge.reveal_vault_secret).not.toHaveBeenCalled();
    });

    it("does not reveal the shared secret or leave pending state after the server mode changes during rotation", async () => {
      markNative();
      const rotation = deferred<EnrollmentView>();
      vi.spyOn(bridge, "enrollment_discover").mockResolvedValue(waitingView());
      vi.spyOn(bridge, "enrollment_rotate_phone").mockReturnValue(rotation.promise);
      const rendered = renderLanding({ mode: "self-hosted" });
      fireEvent.change(screen.getByLabelText("Server URL"), { target: { value: "https://server.example" } });
      fireEvent.click(screen.getByRole("button", { name: "Connect" }));
      fireEvent.click(await screen.findByRole("button", { name: "Enter Vault key phrase on phone" }));
      await waitFor(() => expect(bridge.enrollment_rotate_phone).toHaveBeenCalledWith("shared_secret"));

      rendered.rerender(<SetupLanding mode="hosted" onMode={vi.fn()} enrolledWithoutPhone={false} onJoined={vi.fn()} />);
      await act(async () => rotation.resolve(waitingView()));

      expect(bridge.reveal_vault_secret).not.toHaveBeenCalled();
      expect(pendingIndicator()).not.toBeInTheDocument();
    });

    it("starts and cancels a new computer claimant without using the vault key phrase", async () => {
      vi.spyOn(bridge, "enrollment_discover").mockResolvedValue(view({ stage: "locked" }));
      vi.spyOn(bridge, "enrollment_connect_computer").mockResolvedValue({ stage: "computer_waiting", qrPayload: "computer-qr", awaitingOwner: true, computerClaimant: true });
      vi.spyOn(bridge, "enrollment_cancel_computer").mockResolvedValue();
      renderLanding({ fixedOrigin: "https://community.example", newIdentity: true });

      fireEvent.click(await screen.findByRole("button", { name: "Connect this computer" }));
      expect(await screen.findByRole("img", { name: /computer connection qr code/i })).toBeInTheDocument();
      expect(bridge.enrollment_connect_shared_secret).not.toHaveBeenCalled();
      fireEvent.click(screen.getByRole("button", { name: "Cancel connection" }));
      await waitFor(() => expect(bridge.enrollment_cancel_computer).toHaveBeenCalledOnce());
    });

    it("does not offer a new computer claimant for a locked saved identity", async () => {
      vi.spyOn(bridge, "enrollment_discover").mockResolvedValue(view({ stage: "locked" }));
      renderLanding({ fixedOrigin: "https://community.example" });

      expect(await screen.findByRole("heading", { name: "Unlock this device" })).toBeInTheDocument();
      expect(screen.queryByRole("button", { name: /connect this computer/i })).not.toBeInTheDocument();
    });

    it("offers the native claimant only when discovery reports no saved binding", async () => {
      vi.spyOn(bridge, "enrollment_discover").mockResolvedValue(view({ stage: "locked", canConnectComputer: true }));
      renderLanding({ fixedOrigin: "https://community.example" });

      expect(await screen.findByRole("button", { name: "Connect this computer" })).toBeInTheDocument();
    });

    it("guides a narrow new browser identity to the verified download page before continuing", async () => {
      Object.defineProperty(window, "matchMedia", { configurable: true, value: vi.fn().mockReturnValue({ matches: true, addEventListener: vi.fn(), removeEventListener: vi.fn() }) });
      vi.spyOn(bridge, "enrollment_discover").mockResolvedValue(view({ stage: "locked" }));
      renderLanding({ fixedOrigin: "https://community.example", hostKind: "browser", newIdentity: true, accountUrl: "https://peppy.pro/account" });

      expect(await screen.findByRole("heading", { name: "Use Peppy in this browser" })).toBeInTheDocument();
      expect(screen.getByRole("link", { name: /peppy downloads \(computers\)/i })).toHaveAttribute("href", "https://peppy.pro/account");
      expect(screen.getByText(/cannot scan its own connection code/i)).toBeInTheDocument();
      fireEvent.click(screen.getByRole("button", { name: /continue in this browser/i }));
      expect(await screen.findByRole("button", { name: "Connect this browser" })).toBeInTheDocument();
    });

    it("keeps browser continuation available without an unverified download link", async () => {
      Object.defineProperty(window, "matchMedia", { configurable: true, value: vi.fn().mockReturnValue({ matches: true, addEventListener: vi.fn(), removeEventListener: vi.fn() }) });
      vi.spyOn(bridge, "enrollment_discover").mockResolvedValue(view({ stage: "locked" }));
      renderLanding({ fixedOrigin: "https://community.example", hostKind: "browser", newIdentity: true });

      await screen.findByRole("heading", { name: "Use Peppy in this browser" });
      expect(screen.queryByRole("link", { name: /peppy downloads \(computers\)/i })).not.toBeInTheDocument();
      expect(screen.getByRole("button", { name: /continue in this browser/i })).toBeInTheDocument();
    });

    it("shows a retryable claimant error without claiming approval", async () => {
      vi.spyOn(bridge, "enrollment_discover").mockResolvedValue(view({ stage: "locked" }));
      vi.spyOn(bridge, "enrollment_connect_computer").mockResolvedValue({ stage: "computer_error", awaitingOwner: false, computerClaimant: true });
      renderLanding({ fixedOrigin: "https://community.example", newIdentity: true });

      fireEvent.click(await screen.findByRole("button", { name: "Connect this computer" }));
      expect(await screen.findByRole("heading", { name: /connection needs attention/i })).toBeInTheDocument();
      expect(screen.queryByRole("button", { name: /finish connection/i })).not.toBeInTheDocument();
    });

    it("retries a finishing browser claimant through confirm without polling status", async () => {
      vi.spyOn(bridge, "enrollment_discover").mockResolvedValue(view({ stage: "locked" }));
      vi.spyOn(bridge, "enrollment_connect_computer").mockResolvedValue({ stage: "computer_finishing", awaitingOwner: false, computerClaimant: true, errorCode: "computer_transient" });
      vi.spyOn(bridge, "enrollment_confirm_computer").mockResolvedValue({ stage: "computer_finishing", awaitingOwner: false, computerClaimant: true, errorCode: "computer_transient" });
      vi.spyOn(bridge, "enrollment_computer_status");
      renderLanding({ fixedOrigin: "https://community.example", newIdentity: true });

      fireEvent.click(await screen.findByRole("button", { name: "Connect this computer" }));
      fireEvent.click(await screen.findByRole("button", { name: "Retry finishing" }));

      await waitFor(() => expect(bridge.enrollment_confirm_computer).toHaveBeenCalledOnce());
      expect(bridge.enrollment_computer_status).not.toHaveBeenCalled();
    });

    it("offers reopening, not an activation retry, after a poisoned claimant checkpoint", async () => {
      vi.spyOn(bridge, "enrollment_discover").mockResolvedValue(view({ stage: "locked" }));
      vi.spyOn(bridge, "enrollment_connect_computer").mockResolvedValue({ stage: "computer_error", awaitingOwner: false, computerClaimant: true, errorCode: "computer_reopen_required" });
      renderLanding({ fixedOrigin: "https://community.example", newIdentity: true });

      fireEvent.click(await screen.findByRole("button", { name: "Connect this computer" }));
      expect(await screen.findByRole("button", { name: "Reopen Peppy" })).toBeInTheDocument();
      expect(screen.queryByRole("button", { name: "Start again" })).not.toBeInTheDocument();
    });

    it("opens the connected device after durable claimant completion", async () => {
      vi.spyOn(bridge, "enrollment_discover").mockResolvedValue(view({ stage: "locked" }));
      vi.spyOn(bridge, "enrollment_connect_computer").mockResolvedValue({ stage: "computer_connected", awaitingOwner: false, computerClaimant: true });
      const onJoined = vi.fn();
      renderLanding({ fixedOrigin: "https://community.example", newIdentity: true, onJoined });

      fireEvent.click(await screen.findByRole("button", { name: "Connect this computer" }));
      await waitFor(() => expect(onJoined).toHaveBeenCalledOnce());
    });

    it("explains the browser restart unlock as a visible notice, not a muted hint", async () => {
      vi.spyOn(bridge, "enrollment_discover").mockResolvedValue(view({ stage: "locked" }));
      vi.spyOn(bridge, "enrollment_connect_computer").mockResolvedValue({ stage: "computer_waiting", qrPayload: "computer-qr", awaitingOwner: true, computerClaimant: true });
      renderLanding({ fixedOrigin: "https://community.example", hostKind: "browser", newIdentity: true });

      fireEvent.click(await screen.findByRole("button", { name: "Connect this browser" }));
      const notice = await waitFor(() => {
        const element = document.getElementById("computer-browser-restart-notice");
        expect(element).not.toBeNull();
        return element!;
      });
      expect(notice).toHaveTextContent("After restarting this browser, unlock it with your Vault key phrase. On an already connected phone, use Show Vault key phrase.");
      expect(notice).toHaveClass("setup-notice");
      expect(notice).not.toHaveClass("setup-hint");
    });

    it("shows the approved code with a visible caption and no decline action", async () => {
      vi.spyOn(bridge, "enrollment_discover").mockResolvedValue(view({ stage: "locked" }));
      vi.spyOn(bridge, "enrollment_connect_computer").mockResolvedValue({ stage: "computer_approved", awaitingOwner: false, computerClaimant: true, verificationCode: "654321" });
      renderLanding({ fixedOrigin: "https://community.example", newIdentity: true });

      fireEvent.click(await screen.findByRole("button", { name: "Connect this computer" }));
      expect(await screen.findByRole("group", { name: "Verification code" })).toBeInTheDocument();
      expect(screen.getByText("Verification code", { selector: "p" })).toBeVisible();
      expect(screen.getByLabelText("Verification code: 654321")).toBeInTheDocument();
      expect(screen.getByText(peppyCopy.computer_approved_body)).toBeInTheDocument();
      expect(screen.queryByRole("button", { name: /decline|deny|cancel/i })).not.toBeInTheDocument();
      expect(screen.getByRole("button", { name: "Finish connection" })).toHaveClass("primary-button");
    });

    it("keeps an approved claimant retryable after a transient finish failure without restarting", async () => {
      vi.spyOn(bridge, "enrollment_discover").mockResolvedValue(view({ stage: "locked" }));
      vi.spyOn(bridge, "enrollment_connect_computer").mockResolvedValue({ stage: "computer_approved", awaitingOwner: false, computerClaimant: true, verificationCode: "654321", errorCode: "computer_transient" });
      vi.spyOn(bridge, "enrollment_confirm_computer").mockResolvedValue({ stage: "computer_connected", awaitingOwner: false, computerClaimant: true });
      vi.spyOn(bridge, "enrollment_cancel_computer");
      const onJoined = vi.fn();
      renderLanding({ fixedOrigin: "https://community.example", newIdentity: true, onJoined });

      fireEvent.click(await screen.findByRole("button", { name: "Connect this computer" }));
      const alert = await screen.findByRole("alert");
      expect(alert).toHaveTextContent(peppyCopy.computer_approved_transient_body);
      expect(alert).toHaveTextContent(/check your network connection/i);
      expect(screen.getByLabelText("Verification code: 654321")).toBeInTheDocument();
      expect(screen.queryByRole("button", { name: "Finish connection" })).not.toBeInTheDocument();
      expect(screen.queryByRole("button", { name: /cancel|start again|decline/i })).not.toBeInTheDocument();
      fireEvent.click(screen.getByRole("button", { name: "Retry finishing" }));

      await waitFor(() => expect(bridge.enrollment_confirm_computer).toHaveBeenCalledOnce());
      expect(bridge.enrollment_cancel_computer).not.toHaveBeenCalled();
      await waitFor(() => expect(onJoined).toHaveBeenCalledOnce());
    });

    it("keeps the Vault key phrase fallback secondary beside a computer connect action", async () => {
      vi.spyOn(bridge, "enrollment_discover").mockResolvedValue(view({ stage: "locked" }));
      renderLanding({ fixedOrigin: "https://community.example", newIdentity: true });

      expect(await screen.findByRole("button", { name: "Connect this computer" })).toHaveClass("primary-button");
      expect(screen.getByRole("button", { name: "Enter Vault key phrase" })).toHaveClass("secondary-button");
      expect(screen.getByText(/usually six words, and a custom phrase also works/i)).toBeInTheDocument();
      expect(screen.getByText(/not your account or admin password/i, { selector: "p" })).toBeInTheDocument();
    });

    it("keeps the Vault key phrase as the primary action when no computer connect is offered", async () => {
      vi.spyOn(bridge, "enrollment_discover").mockResolvedValue(view({ stage: "locked" }));
      renderLanding({ fixedOrigin: "https://community.example" });

      expect(await screen.findByRole("button", { name: "Enter Vault key phrase" })).toHaveClass("primary-button");
      expect(screen.queryByText(/usually six words/i)).not.toBeInTheDocument();
    });
  });
});
