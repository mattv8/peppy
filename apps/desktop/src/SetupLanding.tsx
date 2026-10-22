import { useEffect, useRef, useState, type ReactNode } from "react";
import QRCode from "qrcode";
import { BusyIndicator } from "@peppy/desktop-ui";
import { bridge, type EnrollmentView, type HostedAccountView } from "./bridge";
import { peppyCopy, type PeppyCopyKey } from "./generated/peppyCopy";
import "./setup-landing.css";

type Mode = "hosted" | "self-hosted";
type HostedPath = "chooser" | "existing" | "new";
type Route = "checking" | "chooser" | "new" | "enrollment" | "lapsed" | "billing" | "provisioning" | "passphrase";
type Method = "transfer" | "shared_secret";
type Stage = EnrollmentView["stage"];
type InvitationAttempt = "idle" | "starting" | "active" | "stopped";
type PendingOperation = { id: number; label: PeppyCopyKey };
type EnrollmentOperation = { action: (isCurrent: () => boolean) => Promise<EnrollmentView>; retry: () => void; label: PeppyCopyKey; startOver?: boolean; reconcileCreateOwner?: boolean };

const copy = (key: PeppyCopyKey) => peppyCopy[key];
const pollingStages = new Set<Stage>(["waiting_phone", "verify_phone", "phone_approved"]);
const reconciliationStages = new Set<Stage>(["server", "setup", "signin"]);
const pollIntervalMs = 4_000;
const ownerReconcileRetryMs = 1_000;
const maxOwnerStatusRetries = 15;
const invitePollBackoffMs = [1_000, 2_000, 5_000, 10_000];
const qrOptions = { errorCorrectionLevel: "M" as const, margin: 4, width: 512 };
const methodHints: Record<Method, string> = {
  transfer: "Approve only after the codes match. Your phone then receives this device's vault keys directly.",
  shared_secret: "After scanning, your phone asks for the vault shared secret. Enter it there, then compare the codes.",
};

const nativeSetupPromptHint = (mode: "open" | "protected") => {
  const native = typeof window !== "undefined" && "__TAURI_INTERNALS__" in window;
  if (native) {
    return copy(mode === "open" ? "setup_self_hosted_prompts_hint_open" : "setup_self_hosted_prompts_hint_protected");
  }
  return mode === "open"
    ? "Your admin password and vault shared secret will be requested securely."
    : "Your setup code, admin password, and vault shared secret will be requested securely.";
};

// Native transport/host codes and the enrollment domain codes that have their own message.
// Anything outside this list is reported as "unavailable" so arbitrary text never reaches the UI.
const diagnosticErrorCodes = new Set<string>([
  "offline", "network", "local-tls-config", "secure-store-unavailable", "enrollment", "server-rejected",
  "response-too-large", "host-state", "invalid-origin",
  "server-required",
  "credentials-required", "database-key-missing", "database-key-mismatch", "credential-mismatch", "origin-binding",
  "host-config", "server-response", "revoked", "wrong-passphrase", "vault-secret-mismatch", "credential-changed", "enrollment-state", "database-key-conflict",
  "invalid_setup_token", "instance_already_claimed", "invite_invalid", "vault_owner_required",
  "username_taken", "invalid_username", "vault_request_completed", "request_completed_without_material", "vault-secret-unavailable",
  "vault_not_found", "request_not_found", "request_not_approved", "operation_conflict", "admin-password-length",
  "owner-status-pending",
]);
const diagnosticErrorCode = (code: unknown): string => typeof code === "string" && diagnosticErrorCodes.has(code) ? code : "unavailable";
const thrownErrorCode = (error: unknown): string => typeof error === "object" && error !== null && "code" in error ? diagnosticErrorCode(error.code) : "unavailable";

const enrollmentErrorMessage = (errorCode: EnrollmentView["errorCode"]) => {
  if (errorCode === "offline") return "Peppy could not connect to the server. Check the server address, that the server is running, and its HTTPS certificate, then try again.";
  if (errorCode === "network") return "Peppy could not start a network connection to your server. Try again.";
  if (errorCode === "local-tls-config") return "This Peppy build's local HTTPS certificate authority is invalid, so it cannot secure the connection. Rebuild the development app; the server does not need to change.";
  if (errorCode === "secure-store-unavailable") return "Secure storage could not be used: a protected record is invalid or the operating system denied access. Setup stopped to protect your keys.";
  if (errorCode === "server-rejected") return "The server refused this request. Check the server address, then try again.";
  if (errorCode === "response-too-large") return "The server sent a response that is too large for Peppy to accept.";
  if (errorCode === "host-state") return "Peppy's local state is unavailable. Restart Peppy, then try again.";
  if (errorCode === "invalid-origin") return "That server address is not allowed. Check it, then try again.";
  if (errorCode === "server-required") return "This device has no server selected. Choose Peppy Hosted or enter your server address, then try again.";
  if (errorCode === "host-config") return "Peppy's native host configuration could not be read or saved. Try again.";
  if (errorCode === "server-response") return "The server's response could not be read or verified by this device. Check the server and this Peppy build, then try again.";
  if (errorCode === "revoked") return "The server no longer accepts this device's saved credential. It was revoked or is invalid.";
  if (errorCode === "wrong-passphrase") return "That vault shared secret does not match the pending vault. Check it and try again.";
  if (errorCode === "vault-secret-mismatch") return "The vault shared secret saved on this computer does not match this vault, so the phone was not approved. The saved secret was not changed.";
  if (errorCode === "credential-changed") return "The saved credential changed while setup was running. Try again.";
  if (errorCode === "enrollment-state") return "The phone has not finished its connection request yet. Wait for it to finish, then try again.";
  if (errorCode === "database-key-conflict") return "A protected database key already exists on this device and was not replaced. Try again.";
  if (errorCode === "credential-mismatch") return "The saved credential, the server, or this device's local database do not match each other. Check that this device is linked to the right server, then try again.";
  if (errorCode === "credentials-required") return "No saved device credential was found for this server, so this device cannot open its saved data. No local data was changed.";
  if (errorCode === "database-key-missing") return "The protected key for this device's local database is missing. Peppy did not create a new key or change the database.";
  if (errorCode === "database-key-mismatch") return "The protected key does not open this device's local database. No local data was changed.";
  if (errorCode === "origin-binding") return "The saved credential is bound to a different server address than this one. No local data was changed.";
  if (errorCode === "enrollment") return "Enrollment could not be matched to this device's local state or to the server's response. Check this device and the server, then try again.";
  if (errorCode === "invalid_setup_token") return copy("enrollment_error_invalid_setup_token");
  if (errorCode === "instance_already_claimed") return copy("enrollment_error_already_claimed");
  if (errorCode === "invite_invalid") return copy("enrollment_error_invite_invalid");
  if (errorCode === "vault_owner_required") return copy("enrollment_error_vault_owner_required");
  if (errorCode === "username_taken") return "That username is already in use. Choose another.";
  if (errorCode === "invalid_username") return "Choose a valid username.";
  if (errorCode === "vault_request_completed") return "This vault request was already completed. Contact an owner if you need access.";
  if (errorCode === "request_completed_without_material") return "This vault request was already completed. Start a new request to try again.";
  if (errorCode === "admin-password-length") return copy("admin_password_length");
  if (errorCode === "vault-secret-unavailable") return "This device's vault shared secret is not stored. Open Settings, choose Unlock sync natively in Sync encryption, then retry phone approval.";
  if (errorCode === "owner-status-pending") return "This device is set up, but its status is still being confirmed. Try again.";
  return "Peppy could not complete this step. Your server address was kept.";
};

type SelfHostedUrlError = "setup_self_hosted_url_invalid" | "setup_self_hosted_url_credentials" | "setup_self_hosted_url_path" | "setup_self_hosted_url_https_required";
type SelfHostedUrlCheck = { origin: string | null; error: SelfHostedUrlError | null };

const loopbackIpv4Host = /^127\.\d{1,3}\.\d{1,3}\.\d{1,3}$/;
const isLoopbackHost = (host: string) => host === "localhost" || host === "[::1]" || loopbackIpv4Host.test(host);
const rejectSelfHostedUrl = (error: SelfHostedUrlError): SelfHostedUrlCheck => ({ origin: null, error });

// Mirrors the native origin policy: HTTPS anywhere, plain HTTP only for explicit loopback hosts,
// and origin-only values (no credentials, path, query, or fragment) for every scheme.
function checkSelfHostedUrl(value: string): SelfHostedUrlCheck {
  if (!value.trim()) return { origin: null, error: null };
  let url: URL;
  try {
    url = new URL(value);
  } catch {
    return rejectSelfHostedUrl("setup_self_hosted_url_invalid");
  }
  if (url.protocol !== "https:" && url.protocol !== "http:") return rejectSelfHostedUrl("setup_self_hosted_url_invalid");
  if (url.username || url.password) return rejectSelfHostedUrl("setup_self_hosted_url_credentials");
  if (url.pathname !== "/" || url.search || url.hash || /[?#]/.test(value.trim())) return rejectSelfHostedUrl("setup_self_hosted_url_path");
  if (url.protocol === "http:" && !isLoopbackHost(url.hostname)) return rejectSelfHostedUrl("setup_self_hosted_url_https_required");
  return { origin: url.origin, error: null };
}

export const savedSetupMode = (): Mode => window.localStorage.getItem("peppy.setup.mode") === "self-hosted" ? "self-hosted" : "hosted";

function Heading({ children }: { children: ReactNode }) {
  return <h2 tabIndex={-1}>{children}</h2>;
}

function Stage({ stage, setupMode, children }: { stage: Stage; setupMode?: "open" | "protected"; children: ReactNode }) {
  return <div id={`enrollment-${stage}`} className="enrollment-stage" data-stage={stage} data-setup-mode={setupMode}>{children}</div>;
}

export function SetupLanding({ mode, onMode, enrolledWithoutPhone, enrollmentEstablished = false, addingPhone = false, connectionPending = false, onExit, onDeferPhone, onJoined, onInitialEnrollmentStart, fixedOrigin, accountUrl }: {
  mode: Mode;
  onMode(mode: Mode): void;
  enrolledWithoutPhone: boolean;
  addingPhone?: boolean;
  connectionPending?: boolean;
  onExit?(): void;
  enrollmentEstablished?: boolean;
  onDeferPhone?(): void | Promise<void>;
  onJoined(): void | Promise<void>;
  onInitialEnrollmentStart?(): void;
  fixedOrigin?: string;
  accountUrl?: string;
}) {
  const [account, setAccount] = useState<HostedAccountView | null>(null);
  const [accountLoading, setAccountLoading] = useState(!fixedOrigin && mode === "hosted" && !enrolledWithoutPhone);
  const [accountError, setAccountError] = useState(false);
  const [signInError, setSignInError] = useState(false);
  const [hostedPath, setHostedPath] = useState<HostedPath>("chooser");
  const [view, setView] = useState<EnrollmentView | null>(null);
  const [qr, setQr] = useState("");
  const [method, setMethod] = useState<Method>("transfer");
  const [qrRetry, setQrRetry] = useState(0);
  const [verified, setVerified] = useState(false);
  const [busy, setBusy] = useState(false);
  const [pendingOperation, setPendingOperation] = useState<PendingOperation | null>(null);
  const [invitationAttempt, setInvitationAttempt] = useState<InvitationAttempt>("idle");
  const [qrState, setQrState] = useState<"idle" | "generating" | "error">("idle");
  const [sharedSecretError, setSharedSecretError] = useState<string | null>(null);
  const [startOverConfirming, setStartOverConfirming] = useState(false);
  const [startOverAttempted, setStartOverAttempted] = useState(false);
  const [provisionError, setProvisionError] = useState<string | null>(null);
  const [hostedOperation, setHostedOperation] = useState<PeppyCopyKey | null>(null);
  const [ownerStatusChecking, setOwnerStatusChecking] = useState(false);
  const [selfHostedUrl, setSelfHostedUrl] = useState(fixedOrigin ?? "");
  const [connectedOrigin, setConnectedOrigin] = useState<string | null>(fixedOrigin ?? null);
  const [selfHostedUrlBlurred, setSelfHostedUrlBlurred] = useState(false);
  const [signinPath, setSigninPath] = useState<"chooser" | "password" | "invite">("chooser");
  const [username, setUsername] = useState("admin");
  const [vaultOwner, setVaultOwner] = useState("");
  const [inviteLink, setInviteLink] = useState("");
  const [displayName, setDisplayName] = useState("");
  const [vaultUsername, setVaultUsername] = useState("");
  const [inviteError, setInviteError] = useState<string | null>(null);
  const [pageVisible, setPageVisible] = useState(() => document.visibilityState === "visible");
  const bodyRef = useRef<HTMLDivElement>(null);
  const choice = useRef<HTMLButtonElement>(null);
  const modeSelect = useRef<HTMLSelectElement>(null);
  const verifyCheckbox = useRef<HTMLInputElement>(null);
  const createOwnerButton = useRef<HTMLButtonElement>(null);
  const startOverButton = useRef<HTMLButtonElement>(null);
  const startOverConfirmButton = useRef<HTMLButtonElement>(null);
  const inviteLinkInput = useRef<HTMLInputElement>(null);
  const joined = useRef(false);
  const provisioning = useRef(false);
  const discoverStarted = useRef(false);
  const fixedStarted = useRef(false);
  const enrollEpoch = useRef(0);
  const statusRequestEpoch = useRef(0);
  const foregroundStatusRequest = useRef<number | null>(null);
  const ownerStatusRetryTimer = useRef<number | null>(null);
  const ownerStatusRetryAttempts = useRef(0);
  const mutationInFlight = useRef(false);
  const rotatedInvitation = useRef<string | null>(null);
  const lastRotationOutcome = useRef<Stage | null>(null);
  const invitePollEpoch = useRef(0);
  const accountFetchEpoch = useRef(0);
  const signInEpoch = useRef(0);
  const enrollmentActive = useRef(false);
  const mounted = useRef(true);
  const operationId = useRef(0);
  const invitationAttemptRef = useRef<InvitationAttempt>("idle");
  const qrRequestId = useRef(0);
  const stageRef = useRef<Stage | null>(null);
  const retryRef = useRef<() => void>(() => undefined);
  const previousMode = useRef(mode);
  const previousRoute = useRef<Route | null>(null);
  const latestRoutingState = useRef({ enrolledWithoutPhone, addingPhone });
  latestRoutingState.current = { enrolledWithoutPhone, addingPhone };

  const community = Boolean(fixedOrigin) || mode === "self-hosted" || enrolledWithoutPhone || addingPhone;
  const hostedBrowser = Boolean(fixedOrigin && accountUrl);

  const setInvitationState = (next: InvitationAttempt) => {
    invitationAttemptRef.current = next;
    setInvitationAttempt(next);
  };
  const beginPending = (label: PeppyCopyKey) => {
    const id = ++operationId.current;
    setPendingOperation({ id, label });
    setBusy(true);
    return id;
  };
  const endPending = (id: number) => {
    if (!mounted.current || operationId.current !== id) return;
    setPendingOperation(null);
    setBusy(false);
  };

  const apply = (next: EnrollmentView) => {
    if (!mounted.current) return;
    if (stageRef.current === "phone_approved" && (next.stage === "waiting_phone" || next.stage === "verify_phone" || next.stage === "expired")) return;
    enrollmentActive.current = next.stage === "waiting_phone";
    if (next.stage !== stageRef.current) setVerified(false);
    stageRef.current = next.stage;
    setView(next);
  };
  const clearOwnerStatusRetry = () => {
    if (ownerStatusRetryTimer.current !== null) window.clearTimeout(ownerStatusRetryTimer.current);
    ownerStatusRetryTimer.current = null;
    ownerStatusRetryAttempts.current = 0;
  };
  const failedView = (): EnrollmentView => ({ mode: mode === "hosted" ? "hosted" : "community", stage: "error", origin: connectedOrigin ?? undefined, errorCode: "unavailable" });
  const runEnrollment = async ({ action, retry, label, startOver = false, reconcileCreateOwner = false }: EnrollmentOperation): Promise<EnrollmentView | null> => {
    const epoch = ++enrollEpoch.current;
    let result: EnrollmentView | null = null;
    retryRef.current = retry;
    setStartOverAttempted(startOver);
    const pendingId = beginPending(label);
    mutationInFlight.current = true;
    try {
      const next = await action(() => mounted.current && epoch === enrollEpoch.current);
      if (epoch !== enrollEpoch.current) return null;
      result = next;
      apply(next);
      return next;
    } catch (error) {
      if (epoch === enrollEpoch.current) apply({ ...failedView(), errorCode: thrownErrorCode(error) });
      return null;
    } finally {
      if (epoch === enrollEpoch.current) {
        mutationInFlight.current = false;
        if (pendingId !== null) endPending(pendingId);
        const routing = latestRoutingState.current;
        if ((reconcileCreateOwner || routing.enrolledWithoutPhone || routing.addingPhone) && result && reconciliationStages.has(result.stage)) void refreshStatus(true);
      }
    }
  };
  const cancelActive = () => {
    if (!enrollmentActive.current) return;
    enrollmentActive.current = false;
    void bridge.enrollment_cancel().catch(() => undefined);
  };
  const resetEnrollment = () => {
    enrollEpoch.current += 1;
    foregroundStatusRequest.current = null;
    mutationInFlight.current = false;
    clearOwnerStatusRetry();
    invitePollEpoch.current += 1;
    operationId.current += 1;
    cancelActive();
    enrollmentActive.current = false;
    stageRef.current = null;
    setView(null);
    setQr("");
    setVerified(false);
    setBusy(false);
    setPendingOperation(null);
    setInvitationState("idle");
  };
  const refreshAccount = async () => {
    const epoch = ++accountFetchEpoch.current;
    setAccountError(false);
    setAccountLoading(true);
    setHostedOperation("production_pairing_checking");
    try {
      const next = await bridge.hosted_account();
      if (epoch === accountFetchEpoch.current) setAccount(next);
      return next;
    } catch {
      if (epoch === accountFetchEpoch.current) {
        setAccount(null);
        setAccountError(true);
      }
      return null;
    } finally {
      if (epoch === accountFetchEpoch.current) {
        setAccountLoading(false);
        setHostedOperation(null);
      }
    }
  };

  const discover = (origin?: string) => runEnrollment({ action: () => bridge.enrollment_discover(origin), retry: () => discover(origin), label: "production_pairing_connecting" });
  const discoverHosted = () => runEnrollment({
    action: async isCurrent => {
      const origin = await bridge.hosted_origin_url();
      return isCurrent() ? bridge.enrollment_discover(origin) : failedView();
    },
    retry: discoverHosted,
    label: "production_pairing_connecting",
  });
  const createOwner = () => {
    onInitialEnrollmentStart?.();
    return runEnrollment({ action: () => bridge.enrollment_create_owner({ username }), retry: createOwner, label: "production_pairing_connecting", reconcileCreateOwner: true });
  };
  const startOver = () => runEnrollment({
    action: async () => {
      const next = await bridge.enrollment_start_over();
      setStartOverConfirming(false);
      return next;
    }, retry: () => discover(view?.origin ?? connectedOrigin ?? undefined), label: "production_pairing_connecting", startOver: true,
  });
  const signInEnrollment = () => {
    onInitialEnrollmentStart?.();
    return runEnrollment({ action: () => bridge.enrollment_sign_in({ username }), retry: signInEnrollment, label: "production_pairing_connecting" });
  };
  const connectSharedSecret = () => {
    onInitialEnrollmentStart?.();
    return runEnrollment({ action: () => bridge.enrollment_connect_shared_secret(vaultOwner ? { vaultOwner } : undefined), retry: connectSharedSecret, label: "production_pairing_connecting" });
  };
  const beginVaultRequest = () => {
    onInitialEnrollmentStart?.();
    return runEnrollment({ action: async () => {
      setInviteError(null);
      try { return await bridge.enrollment_begin_vault_request({ inviteLink, displayName }); }
      catch (error) {
        if (typeof error === "object" && error !== null && "code" in error && (error as { code?: string }).code === "invite_invalid") {
          setInviteError(copy("enrollment_error_invite_invalid"));
          return { mode: "community", stage: "invite_request" };
        }
        throw error;
      }
    }, retry: beginVaultRequest, label: "production_pairing_connecting" });
  };
  const createVault = () => {
    onInitialEnrollmentStart?.();
    return runEnrollment({ action: () => bridge.enrollment_create_vault({ username: vaultUsername }), retry: createVault, label: "production_pairing_connecting" });
  };
  const addPhone = (next: Method, automatic = false) => {
    if (invitationAttemptRef.current === "stopped" && automatic) return;
    setMethod(next);
    setInvitationState("starting");
    void runEnrollment({ action: () => bridge.enrollment_add_phone(next), retry: () => addPhone(next), label: "production_pairing_preparing" }).then(result => {
      if (!result || invitationAttemptRef.current === "stopped") return;
      setInvitationState(result.stage === "waiting_phone" ? "active" : "stopped");
    });
  };
  const rotatePhone = (next: Method, automatic = false) => {
    setMethod(next);
    return runEnrollment({ action: async () => {
      const outcome = await bridge.enrollment_rotate_phone(next);
      if (automatic) lastRotationOutcome.current = outcome.stage;
      return outcome;
    }, retry: () => { void rotatePhone(next, automatic); }, label: "production_pairing_refreshing" });
  };
  const deferPhone = () => {
    setInvitationState("stopped");
    void runEnrollment({
      action: async isCurrent => {
        const next = await bridge.enrollment_cancel_phone();
        // A phone that already claimed the code keeps the verification step open.
        if (!isCurrent() || next.stage === "verify_phone" || next.stage === "phone_approved") return next;
        await onDeferPhone?.();
        return next;
      },
      retry: deferPhone,
      label: "production_pairing_cancelling",
    });
  };
  const useSharedSecretPhone = () => {
    setSharedSecretError(null);
    setMethod("shared_secret");
    void runEnrollment({
      action: async isCurrent => {
        if (!isCurrent()) return view ?? failedView();
        const next = await bridge.enrollment_rotate_phone("shared_secret");
        if (!isCurrent() || next.stage !== "waiting_phone" || typeof window === "undefined" || !("__TAURI_INTERNALS__" in window)) return next;
        try {
          await bridge.reveal_vault_secret();
        } catch (error) {
          if (isCurrent()) setSharedSecretError(enrollmentErrorMessage(thrownErrorCode(error)));
        }
        return next;
      },
      retry: useSharedSecretPhone,
      label: "production_pairing_revealing",
    });
  };
  const approve = () => {
    if (!verified) return;
    void runEnrollment({ action: () => bridge.enrollment_approve(), retry: approve, label: "production_pairing_approving" });
  };
  const retryStatus = () => {
    clearOwnerStatusRetry();
    void refreshStatus(true);
  };
  const refreshStatus = async (foreground = false) => {
    if (foregroundStatusRequest.current !== null) return;
    if (mutationInFlight.current) return;
    const statusRequest = ++statusRequestEpoch.current;
    const epoch = enrollEpoch.current;
    const pendingId = foreground ? beginPending("production_pairing_checking") : null;
    if (foreground) {
      foregroundStatusRequest.current = statusRequest;
    }
    try {
      const next = await bridge.enrollment_status();
      if (statusRequest !== statusRequestEpoch.current || epoch !== enrollEpoch.current || mutationInFlight.current) return;
      const routing = latestRoutingState.current;
      if (routing.enrolledWithoutPhone && !routing.addingPhone && (next.stage === "setup" || next.stage === "signin")) {
        if (stageRef.current === "setup" || stageRef.current === "signin") {
          stageRef.current = null;
          setView(null);
        }
        setOwnerStatusChecking(true);
        ownerStatusRetryAttempts.current += 1;
        if (ownerStatusRetryAttempts.current >= maxOwnerStatusRetries) {
          setOwnerStatusChecking(false);
          retryRef.current = retryStatus;
          apply({ ...failedView(), errorCode: "owner-status-pending" });
          return;
        }
        if (ownerStatusRetryTimer.current !== null) window.clearTimeout(ownerStatusRetryTimer.current);
        ownerStatusRetryTimer.current = window.setTimeout(() => {
          ownerStatusRetryTimer.current = null;
          void refreshStatus();
        }, ownerReconcileRetryMs);
        return;
      }
      clearOwnerStatusRetry();
      setOwnerStatusChecking(false);
      if ((routing.enrolledWithoutPhone || routing.addingPhone) && next.stage === "server") {
        void discover();
        return;
      }
      if (next.stage === "error") retryRef.current = retryStatus;
      apply(next);
    } catch (error) {
      if (statusRequest !== statusRequestEpoch.current || epoch !== enrollEpoch.current || mutationInFlight.current) return;
      ownerStatusRetryAttempts.current = 0;
      setOwnerStatusChecking(false);
      retryRef.current = retryStatus;
      apply({ ...failedView(), errorCode: thrownErrorCode(error) });
    } finally {
      if (foreground && foregroundStatusRequest.current === statusRequest) {
        foregroundStatusRequest.current = null;
        if (pendingId !== null && statusRequest === statusRequestEpoch.current && epoch === enrollEpoch.current) endPending(pendingId);
      }
    }
  };
  const cancelPairing = () => {
    invitePollEpoch.current += 1;
    setInvitationState("stopped");
    if (addingPhone) {
      const cancelEpoch = ++enrollEpoch.current;
      foregroundStatusRequest.current = null;
      mutationInFlight.current = false;
      enrollmentActive.current = false;
      const pendingId = beginPending("production_pairing_cancelling");
      void bridge.enrollment_cancel().catch(() => undefined).finally(() => {
        endPending(pendingId);
        if (mounted.current && enrollEpoch.current === cancelEpoch) onExit?.();
      });
      return;
    }
    void runEnrollment({ action: async () => {
      await bridge.enrollment_cancel();
      enrollmentActive.current = false;
      return bridge.enrollment_status();
    }, retry: cancelPairing, label: "production_pairing_cancelling" });
  };
  const changeServer = () => {
    enrollEpoch.current += 1;
    operationId.current += 1;
    foregroundStatusRequest.current = null;
    mutationInFlight.current = false;
    clearOwnerStatusRetry();
    cancelActive();
    setBusy(false);
    setPendingOperation(null);
    apply({ mode: "community", stage: "server", origin: connectedOrigin ?? undefined });
  };
  const backToChooser = () => {
    signInEpoch.current += 1;
    setSignInError(false);
    resetEnrollment();
    setHostedPath("chooser");
  };

  const signIn = async () => {
    recordModeChoice("hosted");
    const epoch = ++signInEpoch.current;
    setSignInError(false);
    setHostedOperation("production_pairing_connecting");
    setBusy(true);
    try {
      const next = await bridge.hosted_sign_in("google");
      if (epoch === signInEpoch.current) {
        accountFetchEpoch.current += 1;
        setAccount(next);
      }
    } catch {
      if (epoch === signInEpoch.current) setSignInError(true);
    } finally {
      if (epoch === signInEpoch.current) {
        setBusy(false);
        setHostedOperation(null);
      }
    }
  };

  const invalidateAccountFlows = () => {
    accountFetchEpoch.current += 1;
    signInEpoch.current += 1;
  };

  useEffect(() => {
    mounted.current = true;
    return () => {
      mounted.current = false;
    invalidateAccountFlows();
    enrollEpoch.current += 1;
    foregroundStatusRequest.current = null;
    mutationInFlight.current = false;
    clearOwnerStatusRetry();
    cancelActive();
    };
  }, []);
  useEffect(() => {
    if (fixedOrigin || previousMode.current === mode) return;
    previousMode.current = mode;
    invalidateAccountFlows();
    resetEnrollment();
    setHostedPath("chooser");
    setAccount(null);
    setAccountError(false);
    setAccountLoading(mode === "hosted");
    setConnectedOrigin(null);
    setSelfHostedUrlBlurred(false);
  }, [fixedOrigin, mode]);
  useEffect(() => {
    if (fixedOrigin || mode !== "hosted" || enrolledWithoutPhone) return;
    void refreshAccount();
    return () => {
      accountFetchEpoch.current += 1;
    };
  }, [fixedOrigin, mode, enrolledWithoutPhone]);
  useEffect(() => {
    if (!fixedOrigin || fixedStarted.current) return;
    fixedStarted.current = true;
    void discover(fixedOrigin);
  }, [fixedOrigin]);
  useEffect(() => {
    if (!enrolledWithoutPhone && !addingPhone) return;
    setOwnerStatusChecking(true);
    void refreshStatus(true);
  }, [addingPhone, enrolledWithoutPhone, enrollmentEstablished]);

  const route: Route = addingPhone ? "enrollment" : accountLoading && account === null ? "checking"
    : account?.signedIn
      ? account.hasVault && account.resumable ? "provisioning"
        : account.classification === "lapsed" ? "lapsed"
          : account.hasVault ? "enrollment"
            : account.access !== "read_write" ? "billing"
              : account.resumable || account.classification === "provisioning" ? "provisioning"
                : "passphrase"
      : hostedPath === "existing" ? "enrollment" : hostedPath === "new" ? "new" : "chooser";

  useEffect(() => {
    if (addingPhone || community || route !== "enrollment" || discoverStarted.current) return;
    discoverStarted.current = true;
    void discoverHosted();
  }, [addingPhone, community, route]);
  useEffect(() => {
    if (route !== "enrollment") discoverStarted.current = false;
  }, [route]);
  useEffect(() => {
    const leftEnrollment = previousRoute.current === "enrollment" && route !== "enrollment";
    previousRoute.current = route;
    if (leftEnrollment) resetEnrollment();
  }, [route]);
  useEffect(() => {
    if (route !== "provisioning" || provisioning.current) return;
    provisioning.current = true;
    provision();
  }, [route]);
  useEffect(() => {
    if (route !== "billing" && route !== "lapsed") return;
    const onFocus = () => void refreshAccount();
    window.addEventListener("focus", onFocus);
    return () => window.removeEventListener("focus", onFocus);
  }, [route]);

  const stage = view?.stage ?? null;
  const invitationCanStart = invitationAttempt === "idle" || invitationAttempt === "stopped";
  const automaticPhoneInvitationPending = stage === "ready" && Boolean(view?.owner)
    && (addingPhone || view?.hasGateway === false)
    && invitationAttempt === "idle";
  useEffect(() => {
    if (stage !== "ready" || !view?.owner || (!addingPhone && view.hasGateway !== false) || invitationAttemptRef.current !== "idle" || mutationInFlight.current) return;
    addPhone("transfer", true);
  }, [stage, view?.owner, view?.hasGateway]);
  useEffect(() => {
    if (stage !== "ready" || !view?.owner || addingPhone || view.hasGateway !== undefined) return;
    let active = true;
    let timer: number | undefined;
    const pollGatewayStatus = async () => {
      await refreshStatus();
      if (active) timer = window.setTimeout(() => void pollGatewayStatus(), pollIntervalMs);
    };
    timer = window.setTimeout(() => void pollGatewayStatus(), pollIntervalMs);
    return () => {
      active = false;
      if (timer !== undefined) window.clearTimeout(timer);
    };
  }, [addingPhone, stage, view?.hasGateway, view?.owner]);
  useEffect(() => {
    if (stage !== "locked") setVaultOwner("");
    if (stage !== "signin") setSigninPath("chooser");
  }, [stage]);
  useEffect(() => {
    if (!startOverConfirming) return;
    startOverConfirmButton.current?.focus();
  }, [startOverConfirming]);
  useEffect(() => {
    if (stage !== "setup" || !view?.ownerResumePending) setStartOverConfirming(false);
    if (stage === "setup" && !view?.ownerResumePending && startOverAttempted) {
      requestAnimationFrame(() => createOwnerButton.current?.focus());
      setStartOverAttempted(false);
    }
  }, [stage, startOverAttempted, view?.ownerResumePending]);
  const cancelStartOver = () => {
    setStartOverConfirming(false);
    requestAnimationFrame(() => startOverButton.current?.focus());
  };
  useEffect(() => {
    if (stage !== "signin" || signinPath !== "chooser" || !bridge.enrollment_pending_invite) return;
    let live = true;
    void bridge.enrollment_pending_invite().then(link => {
      if (live && link) {
        setInviteLink(link);
        setSigninPath("invite");
        requestAnimationFrame(() => inviteLinkInput.current?.focus());
      }
    }).catch(() => undefined);
    return () => { live = false; };
  }, [stage, signinPath]);
  useEffect(() => {
    const updateVisibility = () => setPageVisible(document.visibilityState === "visible");
    document.addEventListener("visibilitychange", updateVisibility);
    return () => document.removeEventListener("visibilitychange", updateVisibility);
  }, []);
  useEffect(() => {
    if (stage !== "invite_waiting" || !pageVisible) return;
    const epoch = ++invitePollEpoch.current;
    let timer: number | undefined;
    let attempt = 0;
    const poll = async () => {
      if (epoch !== invitePollEpoch.current || document.visibilityState !== "visible") return;
      try {
        const next = await bridge.enrollment_vault_request_status();
        if (epoch !== invitePollEpoch.current) return;
        attempt = 0;
        apply(next);
        if (next.stage === "invite_waiting") timer = window.setTimeout(poll, 0);
      } catch {
        if (epoch !== invitePollEpoch.current) return;
        timer = window.setTimeout(poll, invitePollBackoffMs[Math.min(attempt++, invitePollBackoffMs.length - 1)]);
      }
    };
    void poll();
    return () => { invitePollEpoch.current += 1; if (timer !== undefined) window.clearTimeout(timer); };
  }, [pageVisible, stage]);
  const payload = stage === "waiting_phone" ? view?.qrPayload : undefined;
  useEffect(() => {
    if (payload && payload !== rotatedInvitation.current) {
      rotatedInvitation.current = null;
      lastRotationOutcome.current = null;
    }
  }, [payload]);
  useEffect(() => {
    qrRequestId.current += 1;
    const requestId = qrRequestId.current;
    if (!payload) {
      setQr("");
      setQrState("idle");
      return;
    }
    setQr("");
    let live = true;
    setQrState("generating");
    void QRCode.toDataURL(payload, qrOptions).then(url => {
      if (live && requestId === qrRequestId.current) {
        setQr(url);
        setQrState("idle");
      }
    }).catch(() => {
      if (live && requestId === qrRequestId.current) {
        setQr("");
        setQrState("error");
      }
    });
    return () => {
      live = false;
    };
  }, [payload, qrRetry]);
  useEffect(() => {
    if (!stage || !pollingStages.has(stage)) return;
    const timer = window.setInterval(() => {
      void refreshStatus();
    }, pollIntervalMs);
    return () => window.clearInterval(timer);
  }, [stage]);
  useEffect(() => {
    const invitation = stage === "waiting_phone" ? view?.qrPayload : stage === "expired" ? `expired:${view?.qrPayload ?? ""}` : null;
    const nearExpiry = stage === "waiting_phone" && (view?.expiresInSeconds ?? Infinity) <= 15;
    if (!invitation || invitationAttemptRef.current === "stopped" || (!nearExpiry && stage !== "expired") || mutationInFlight.current || rotatedInvitation.current === invitation) return;
    if (stage === "expired" && lastRotationOutcome.current === "expired") return;
    rotatedInvitation.current = invitation;
    void rotatePhone(method, true);
  }, [stage, view?.expiresInSeconds, view?.qrPayload, method]);
  const joinedNow = stage === "phone_connected" || (!addingPhone && stage === "ready" && Boolean(view?.hasGateway));
  useEffect(() => {
    if (!joinedNow || joined.current) return;
    joined.current = true;
    const pendingId = beginPending("production_pairing_finishing");
    void (async () => {
      try {
        await onJoined();
      } catch (error) {
        joined.current = false;
        retryRef.current = () => apply({ mode: view?.mode ?? (mode === "hosted" ? "hosted" : "community"), stage: "phone_connected" });
        apply({ ...failedView(), errorCode: thrownErrorCode(error) });
      } finally {
        endPending(pendingId);
      }
    })();
  }, [joinedNow, onJoined]);
  useEffect(() => {
    if (route === "checking") return;
    if (!community && mode === "hosted" && route === "chooser") {
      choice.current?.focus();
      return;
    }
    requestAnimationFrame(() => {
      if (stage === "verify_phone") verifyCheckbox.current?.focus();
      else if (!fixedOrigin && mode === "self-hosted" && !connectedOrigin) modeSelect.current?.focus();
      else bodyRef.current?.querySelector<HTMLElement>("h2")?.focus();
    });
  }, [fixedOrigin, mode, route, stage, connectedOrigin, community]);

  const recordModeChoice = (next: Mode) => {
    if (!fixedOrigin) window.localStorage.setItem("peppy.setup.mode", next);
  };
  const selectMode = (next: Mode) => {
    recordModeChoice(next);
    if (next !== mode) {
      resetEnrollment();
      setHostedPath("chooser");
      setConnectedOrigin(null);
    }
    onMode(next);
  };
  const chooseExisting = () => {
    recordModeChoice("hosted");
    setHostedPath("existing");
  };
  const chooseNew = () => {
    recordModeChoice("hosted");
    setHostedPath("new");
  };
  const startProvisioning = () => {
    recordModeChoice("hosted");
    provision();
  };
  const provision = () => {
    setProvisionError(null);
    setHostedOperation("hosted_create_vault_progress");
    setBusy(true);
    void bridge.hosted_provision().catch(error => setProvisionError(thrownErrorCode(error))).finally(() => {
      setBusy(false);
      setHostedOperation(null);
      void refreshAccount();
    });
  };
  const connectSelfHosted = () => {
    recordModeChoice("self-hosted");
    setConnectedOrigin(validOrigin);
    void discover(validOrigin ?? undefined);
  };

  const selfHostedCheck = checkSelfHostedUrl(selfHostedUrl);
  const validOrigin = fixedOrigin ?? selfHostedCheck.origin;
  const selfHostedUrlError = selfHostedUrlBlurred ? selfHostedCheck.error : null;
  const showUrlForm = !fixedOrigin && !enrolledWithoutPhone && mode === "self-hosted" && (view === null || view.stage === "server" || view.stage === "error");

  const renderEnrollment = (): ReactNode => {
    const back = mode === "hosted" && !account?.signedIn && !community
      ? <div className="enrollment-actions"><button id="setup-back-button" className="secondary-button" disabled={busy && view?.stage === "verify_phone"} onClick={backToChooser}>{copy("back")}</button></div>
      : null;
    return (
      <>
        {view ? renderStage(view) : <p id="enrollment-checking" role="status">Checking this server…</p>}
        {back}
      </>
    );
  };

  const renderInviteRequestFields = () => {
    const inviteLinkDescription = inviteError ? "enrollment-invite-link-error enrollment-invite-link-hint" : "enrollment-invite-link-hint";
    return <>
      <div id="enrollment-invite-link-field" className="setup-url-field">
        <label>{copy("enrollment_invite_link_label")}<input ref={inviteLinkInput} id="enrollment-invite-link" type="url" autoComplete="off" value={inviteLink} onChange={event => setInviteLink(event.target.value)} aria-invalid={inviteError ? "true" : undefined} aria-describedby={inviteLinkDescription} /></label>
        {inviteError && <span id="enrollment-invite-link-error" className="setup-field-error" aria-live="polite">{inviteError}</span>}
        <span id="enrollment-invite-link-hint" className="setup-hint">{copy("enrollment_invite_link_hint")}</span>
      </div>
      <div id="enrollment-display-name-field" className="setup-url-field">
        <label>{copy("enrollment_display_name_label")}<input id="enrollment-display-name" type="text" maxLength={64} autoComplete="name" value={displayName} onChange={event => setDisplayName(event.target.value)} aria-describedby="enrollment-display-name-hint" /></label>
        <span id="enrollment-display-name-hint" className="setup-hint">{copy("enrollment_display_name_hint")}</span>
      </div>
    </>;
  };

  const renderStage = (view: EnrollmentView): ReactNode => {
    const hosted = view.mode === "hosted";
    switch (view.stage) {
      case "server":
        return hosted ? (
          <Stage stage="server">
            <Heading>Connecting to Peppy Hosted</Heading>
            <div className="enrollment-actions"><button className="secondary-button" disabled={busy} onClick={() => void discoverHosted()}>{copy("try_again")}</button></div>
          </Stage>
        ) : null;
      case "setup":
        return hosted ? (
          <Stage stage="setup">
            <Heading>Create your vault</Heading>
            <p>Peppy will create your vault and a shared secret. Each one is confirmed in a secure prompt.</p>
            <div className="enrollment-actions">
              <button id="enrollment-create-owner-button" className="primary-button" disabled={busy} onClick={() => void createOwner()}>Create vault</button>
              <button id="enrollment-cancel-setup-button" className="secondary-button" disabled={busy} onClick={changeServer}>Cancel</button>
            </div>
          </Stage>
        ) : (
          <Stage stage="setup" setupMode={view.setupCodeRequired === false ? "open" : "protected"}>
            <Heading>Set up your server</Heading>
            {view.setupCodeRequired === false ? (
              <>
                <p className="setup-hint">{nativeSetupPromptHint("open")}</p>
              </>
            ) : (
              <>
                <p>{copy("enrollment_setup_protected")}</p>
                <p className="setup-hint">{nativeSetupPromptHint("protected")}</p>
              </>
            )}
            {view.ownerResumePending && <>
              <p id="enrollment-resume-notice" className="setup-hint">{copy("setup_owner_resume_body")}</p>
              {startOverConfirming ? (
                <div id="enrollment-start-over-confirm" role="group" aria-label={copy("setup_owner_resume_confirm_label")}>
                  <p id="enrollment-start-over-confirm-body" className="setup-hint">{copy("setup_owner_resume_confirm_body")}</p>
                  <div className="enrollment-actions">
                    <button ref={startOverConfirmButton} id="enrollment-start-over-confirm-button" className="primary-button" disabled={busy} aria-describedby="enrollment-start-over-confirm-body" onClick={() => void startOver()}>{copy("setup_owner_resume_confirm_yes")}</button>
                    <button id="enrollment-start-over-cancel-button" className="secondary-button" disabled={busy} onClick={cancelStartOver}>{copy("setup_owner_resume_confirm_no")}</button>
                  </div>
                </div>
              ) : <button ref={startOverButton} id="enrollment-start-over-button" className="secondary-button" disabled={busy} onClick={() => setStartOverConfirming(true)}>{copy("setup_owner_resume_start_over")}</button>}
            </>}
            <div id="enrollment-setup-username-field" className="setup-url-field"><label>{copy("enrollment_username_label")}<input id="enrollment-setup-username" type="text" autoComplete="username" value={username} onChange={event => setUsername(event.target.value)} aria-describedby="enrollment-setup-username-hint" /></label><span id="enrollment-setup-username-hint" className="setup-hint">{copy("enrollment_setup_username_hint")}</span></div>
            <div className="enrollment-actions">
              <button ref={createOwnerButton} id="enrollment-create-owner-button" className="primary-button" disabled={busy} onClick={() => void createOwner()}>Create server</button>
              <button id="enrollment-cancel-setup-button" className="secondary-button" disabled={busy} onClick={changeServer}>Cancel</button>
            </div>
          </Stage>
        );
      case "signin":
        if (!hosted && signinPath === "password") return <Stage stage="signin"><Heading>Sign in to your server</Heading><div id="enrollment-signin-username-field" className="setup-url-field"><label>{copy("enrollment_username_label")}<input id="enrollment-signin-username" type="text" autoComplete="username" value={username} onChange={event => setUsername(event.target.value)} aria-describedby="enrollment-signin-username-hint" /></label><span id="enrollment-signin-username-hint" className="setup-hint">{copy("enrollment_signin_username_hint")}</span></div><div className="enrollment-actions"><button id="enrollment-signin-button" className="primary-button" disabled={busy} onClick={() => void signInEnrollment()}>{copy("enrollment_signin_cta")}</button><button id="enrollment-signin-back-button" className="secondary-button" onClick={() => setSigninPath("chooser")}>{copy("back")}</button></div></Stage>;
        if (!hosted && signinPath === "invite") return <Stage stage="invite_request"><Heading>{copy("enrollment_invite_request_headline")}</Heading><p>{copy("enrollment_invite_request_body")}</p>{renderInviteRequestFields()}<div className="enrollment-actions"><button id="enrollment-invite-submit-button" className="primary-button" disabled={busy || !inviteLink || !displayName} onClick={() => void beginVaultRequest()}>{copy("enrollment_invite_request_cta")}</button><button id="enrollment-invite-cancel-button" className="secondary-button" onClick={() => setSigninPath("chooser")}>{copy("enrollment_invite_cancel_cta")}</button></div></Stage>;
        return (
          <Stage stage="signin">
            <Heading>{hosted ? copy("hosted_sign_in_headline") : "Sign in to your server"}</Heading>
            <p>{hosted ? "Sign in to your Peppy account to continue. Signing in does not unlock your vault." : copy("enrollment_signin_body")}</p>
            <div className={hosted ? "enrollment-actions" : "chooser-buttons"}>
              {!hosted && view.ownerResumePending && <button id="enrollment-signin-resume-button" className="chooser-button" disabled={busy} onClick={() => void createOwner()}><span className="chooser-button-label">{copy("setup_owner_resume_signin_label")}</span><span className="chooser-button-hint">{copy("setup_owner_resume_signin_hint")}</span></button>}
              <button id="enrollment-signin-password-button" className={hosted ? "primary-button" : "chooser-button"} disabled={busy} onClick={() => hosted ? void signInEnrollment() : setSigninPath("password")}>{hosted ? "Sign in" : <><span className="chooser-button-label">{copy("enrollment_signin_password_label")}</span><span className="chooser-button-hint">{copy("enrollment_signin_password_hint")}</span></>}</button>
              {!hosted && <button id="enrollment-signin-connect-button" className="chooser-button" disabled={busy} onClick={() => void connectSharedSecret()}><span className="chooser-button-label">{copy("enrollment_signin_connect_label")}</span><span className="chooser-button-hint">{copy("enrollment_signin_connect_hint")}</span></button>}
              {!hosted && <button id="enrollment-signin-invite-button" className="chooser-button" disabled={busy} onClick={() => setSigninPath("invite")}><span className="chooser-button-label">{copy("enrollment_signin_invite_label")}</span><span className="chooser-button-hint">{copy("enrollment_signin_invite_hint")}</span></button>}
            </div>
          </Stage>
        );
      case "locked":
        return (
          <Stage stage="locked">
            <Heading>Connect this device</Heading>
            <p>Enter the shared secret from your server owner in the secure prompt. Signing in alone does not unlock messages.</p>
            {view.vaultOwnerRequired && <div id="enrollment-vault-owner-field" className="setup-url-field"><label>{copy("enrollment_vault_owner_label")}<input id="enrollment-vault-owner" type="text" autoComplete="username" value={vaultOwner} onChange={event => setVaultOwner(event.target.value)} /></label><span className="setup-hint">{copy("enrollment_vault_owner_hint")}</span></div>}
            <div className="enrollment-actions">
              <button id="enrollment-shared-secret-button" className="primary-button" disabled={busy} onClick={() => void connectSharedSecret()}>Enter shared secret</button>
            </div>
          </Stage>
        );
      case "invite_request":
        return <Stage stage="invite_request"><Heading>{copy("enrollment_invite_request_headline")}</Heading><p>{copy("enrollment_invite_request_body")}</p>{renderInviteRequestFields()}<div className="enrollment-actions"><button id="enrollment-invite-submit-button" className="primary-button" disabled={busy || !inviteLink || !displayName} onClick={() => void beginVaultRequest()}>{copy("enrollment_invite_request_cta")}</button><button id="enrollment-invite-cancel-button" className="secondary-button" onClick={() => void cancelPairing()}>{copy("enrollment_invite_cancel_cta")}</button></div></Stage>;
      case "invite_waiting":
        return <Stage stage="invite_waiting"><Heading>{copy("enrollment_invite_waiting_headline")}</Heading><p>{copy("enrollment_invite_waiting_body")}</p><p className="setup-hint">{copy("enrollment_invite_waiting_hint")}</p><div id="enrollment-invite-waiting-status" className="visually-hidden" aria-live="polite" aria-atomic="true">{copy("enrollment_invite_poll_status")}</div><div className="enrollment-actions"><button id="enrollment-invite-cancel-button" className="secondary-button" disabled={busy} onClick={() => void cancelPairing()}>{copy("enrollment_invite_cancel_cta")}</button></div></Stage>;
      case "invite_approved":
        return <Stage stage="invite_approved"><Heading>{copy("enrollment_invite_approved_headline")}</Heading><p>{copy("enrollment_invite_approved_body")}</p>{view.errorCode && <p id="enrollment-vault-error" role="alert">{enrollmentErrorMessage(view.errorCode)}</p>}<div id="enrollment-vault-username-field" className="setup-url-field"><label>{copy("enrollment_username_label")}<input id="enrollment-vault-username" type="text" autoComplete="username" value={vaultUsername} onChange={event => setVaultUsername(event.target.value)} aria-describedby="enrollment-vault-username-hint" /></label><span id="enrollment-vault-username-hint" className="setup-hint">{copy("enrollment_vault_username_hint")}</span></div><p className="setup-hint">{copy("enrollment_invite_approved_secure_hint")}</p><div className="enrollment-actions"><button id="enrollment-create-vault-button" className="primary-button" disabled={busy || !vaultUsername} onClick={() => void createVault()}>{copy("enrollment_create_vault_cta")}</button><button id="enrollment-invite-cancel-button" className="secondary-button" disabled={busy} onClick={() => void cancelPairing()}>{copy("enrollment_invite_cancel_cta")}</button></div></Stage>;
      case "invite_denied":
      case "invite_expired": {
        const denied = view.stage === "invite_denied";
        return <Stage stage={view.stage}><Heading>{copy(denied ? "enrollment_invite_denied_headline" : "enrollment_invite_expired_headline")}</Heading><p role="alert">{copy(denied ? "enrollment_invite_denied_body" : "enrollment_invite_expired_body")}</p><div className="enrollment-actions"><button id="enrollment-invite-restart-button" className="secondary-button" onClick={() => void cancelPairing()}>{copy("enrollment_invite_restart_cta")}</button></div></Stage>;
      }
      case "ready":
        if (view.hasGateway && !addingPhone) return <Stage stage="ready"><p role="status">Phone connected.</p></Stage>;
        if (!view.owner) {
          return (
            <Stage stage="ready">
              <Heading>Waiting for an owner</Heading>
              <p>An owner of this server can connect a phone. Ask them to add your phone from Peppy.</p>
            </Stage>
          );
        }
        return (
          <Stage stage="ready">
            <Heading>Connect your phone</Heading>
            <p>Connect a phone to start sending messages. Permissions are granted on the phone, not here.</p>
            {invitationCanStart && !automaticPhoneInvitationPending && !busy && <p id="enrollment-idle-status">{copy("production_pairing_idle")}</p>}
            {invitationCanStart && !automaticPhoneInvitationPending && (view.hasGateway !== undefined || addingPhone) && !mutationInFlight.current && !connectionPending && !busy && <div className="enrollment-actions">
              <button id="enrollment-show-connection-code-button" className="primary-button" disabled={busy} onClick={() => addPhone(method)}>Show connection code</button>
              <button id="enrollment-ready-defer-phone-button" className="secondary-button" disabled={busy} onClick={() => void deferPhone()}>Set up phone later</button>
            </div>}
            {invitationAttempt === "idle" && !addingPhone && view.hasGateway === undefined && onDeferPhone && !connectionPending && !busy && <div className="enrollment-actions">
              <button id="enrollment-unknown-roster-defer-phone-button" className="secondary-button" disabled={busy} onClick={() => void deferPhone()}>Set up phone later</button>
            </div>}
          </Stage>
        );
      case "waiting_phone":
        return (
          <Stage stage="waiting_phone">
            <Heading>Connect your phone</Heading>
            <p>Open Peppy on your phone and scan this code.</p>
            {qrState === "generating" && <BusyIndicator id="enrollment-qr-generating" label={copy("production_pairing_qr_generating")} />}
            {qrState === "error" && <p id="enrollment-qr-error" className="setup-field-error" role="alert">{copy("production_pairing_qr_error")} <button className="secondary-button" disabled={busy} onClick={() => setQrRetry(current => current + 1)}>{copy("try_again")}</button></p>}
            {qr && (
              <div id="enrollment-qr-hero">
                <img id="enrollment-qr-image" src={qr} alt="" aria-hidden="true" />
                <span className="visually-hidden" role="img" aria-label={copy("production_pairing_qr_accessibility")} />
                {view.expiresInSeconds !== undefined && <span id="enrollment-countdown" role="timer" aria-live="off" aria-label={`${view.expiresInSeconds} seconds remaining`}>Expires in {view.expiresInSeconds}s</span>}
              </div>
            )}
            <p className="setup-hint">{methodHints[method]}</p>
            {sharedSecretError && <p id="enrollment-shared-secret-error" className="setup-field-error" role="alert">{sharedSecretError}</p>}
            <p className="setup-hint">This QR connects your phone to this server. It does not grant any permissions by itself.</p>
            <div className="enrollment-actions">
              <button id="enrollment-cancel-button" className="secondary-button" disabled={busy} onClick={() => void cancelPairing()}>Cancel</button>
              <button id="enrollment-shared-secret-phone-button" className="secondary-button" disabled={busy} onClick={useSharedSecretPhone}>Enter shared secret on phone</button>
              <button id="enrollment-defer-phone-button" className="secondary-button" disabled={busy} onClick={deferPhone}>Set up phone later</button>
            </div>
          </Stage>
        );
      case "verify_phone":
        return (
          <Stage stage="verify_phone">
            <Heading>Verify the code</Heading>
            <p>Compare this code with the code shown in Peppy on your phone. Approve only if they match.</p>
            <output id="enrollment-verification-code" aria-label={`Verification code: ${view.verificationCode ?? ""}`}>{view.verificationCode}</output>
            <label className="settings-check">
              <input ref={verifyCheckbox} id="enrollment-verify-checkbox" type="checkbox" checked={verified} disabled={busy} onChange={event => setVerified(event.target.checked)} /> The codes match on both devices.
            </label>
            <div className="enrollment-actions">
              <button id="enrollment-approve-button" className="primary-button" disabled={!verified || busy} onClick={approve}>Confirm and approve</button>
              <button id="enrollment-cancel-button" className="secondary-button" disabled={busy} onClick={() => void cancelPairing()}>Cancel</button>
            </div>
          </Stage>
        );
      case "phone_approved":
        return <Stage stage="phone_approved"><Heading>Phone approved</Heading><BusyIndicator id="enrollment-phone-approved-status" label={copy("owner_phone_approved_body")} />{onExit && <div className="enrollment-actions"><button id="enrollment-approved-close-button" className="secondary-button" disabled={busy} onClick={onExit}>Close</button></div>}</Stage>;
      case "phone_connected":
        return <Stage stage="phone_connected"><Heading>Phone connected</Heading><p role="status">Finish setup on your phone.</p></Stage>;
      case "expired":
        return (
          <Stage stage="expired">
            <Heading>Code expired</Heading>
            <p role="alert">This code expired. Start again to get a new one.</p>
            <div className="enrollment-actions">
              <button id="enrollment-restart-button" className="primary-button" disabled={busy} onClick={() => addPhone(method)}>Start again</button>
              <button id="enrollment-cancel-button" className="secondary-button" disabled={busy} onClick={() => void cancelPairing()}>Cancel</button>
            </div>
          </Stage>
        );
      case "error":
        if (view.errorCode === "vault_request_completed" || view.errorCode === "request_completed_without_material") {
          return <Stage stage="error"><Heading>Vault request completed</Heading><p id="enrollment-error" role="alert">{enrollmentErrorMessage(view.errorCode)}</p><div className="enrollment-actions"><button id="enrollment-invite-restart-button" className="secondary-button" disabled={busy} onClick={() => void cancelPairing()}>{copy("enrollment_invite_restart_cta")}</button></div></Stage>;
        }
        return (
          <Stage stage="error">
            <Heading>Something went wrong</Heading>
            <p id="enrollment-error" role="alert">{startOverAttempted && view.errorCode === "instance_already_claimed" ? copy("setup_owner_resume_claimed") : startOverAttempted && view.errorCode === "operation_conflict" ? copy("setup_owner_resume_conflict") : enrollmentErrorMessage(view.errorCode)}</p>
            <p id="enrollment-error-code" className="setup-hint" data-error-code={diagnosticErrorCode(view.errorCode)}>Error code: {diagnosticErrorCode(view.errorCode)}</p>
            <div className="enrollment-actions">
              <button id="enrollment-retry-button" className="primary-button" disabled={busy} onClick={() => retryRef.current()}>Retry</button>
            </div>
          </Stage>
        );
      default:
        return null;
    }
  };

  const statusMessage = pendingOperation || hostedOperation || ownerStatusChecking ? "" : stage === "setup" && view?.ownerResumePending ? copy("setup_owner_resume_status")
    : stage === "waiting_phone" ? "Scan this code with your phone."
    : stage === "verify_phone" ? "Compare the verification code before approving."
      : stage === "phone_connected" ? "Phone connected. Finish setup on your phone."
        : stage === "expired" ? "This code expired."
          : stage === "invite_waiting" ? copy("enrollment_invite_waiting_live_status")
          : stage === "invite_approved" ? copy("enrollment_invite_approved_live_status")
          : stage === "invite_denied" ? copy("enrollment_invite_denied_live_status")
          : stage === "invite_expired" ? copy("enrollment_invite_expired_live_status")
          : stage === "error" ? "Something went wrong."
            : "";

  const hostedPanels = (
    <>
      {route === "checking" && <section id="hosted-account-checking" aria-busy="true"><h2 tabIndex={-1}>{copy("hosted_account_checking")}</h2></section>}
      {route === "chooser" && <div id="hosted-path-chooser">{accountError && <p id="hosted-account-check-error" role="alert">{copy("hosted_account_check_failed")} <button className="secondary-button" onClick={() => void refreshAccount()}>{copy("try_again")}</button></p>}<div className="chooser-buttons"><button ref={choice} id="hosted-choice-existing" className="chooser-button" onClick={chooseExisting}><span className="chooser-button-label">{copy("setup_choice_existing")}</span><span className="chooser-button-hint">{copy("setup_choice_existing_hint")}</span></button><button id="hosted-choice-new" className="chooser-button" onClick={chooseNew}><span className="chooser-button-label">{copy("setup_choice_new")}</span><span className="chooser-button-hint">{copy("setup_choice_new_hint")}</span></button></div></div>}
      {route === "new" && <section id="hosted-signin-panel" data-section="signin"><h2 tabIndex={-1}>{copy("hosted_sign_in_headline")}</h2><p>{copy("hosted_sign_in_body")}</p><button id="hosted-signin-google-button" className="primary-button" disabled={!account?.available || busy} onClick={() => void signIn()}>{copy("hosted_sign_in_google")}</button>{account && !account.available && <p id="hosted-signin-unavailable" role="alert">{copy("production_hosted_unavailable")}</p>}{accountError && <p id="hosted-account-check-error" role="alert">{copy("hosted_account_check_failed")} <button className="secondary-button" onClick={() => void refreshAccount()}>{copy("try_again")}</button></p>}{signInError && <p id="hosted-signin-error" role="alert">{copy("production_signin_failed")} <button className="secondary-button" onClick={() => void signIn()}>{copy("try_again")}</button></p>}<button id="setup-back-button" className="secondary-button" onClick={backToChooser}>{copy("back")}</button></section>}
      {route === "enrollment" && <div id="enrollment-panel">{renderEnrollment()}</div>}
      {route === "lapsed" && <section id="hosted-lapsed-card"><h2 tabIndex={-1}>{copy("hosted_lapsed_headline")}</h2><p>{copy("hosted_lapsed_body")}</p><button className="primary-button" onClick={() => void bridge.hosted_open_billing()}>{copy("production_billing_open")}</button><button className="secondary-button" onClick={() => void refreshAccount()}>{copy("production_billing_check")}</button><button className="secondary-button" onClick={() => void bridge.hosted_sign_out()}>{copy("settings_server_sign_out")}</button></section>}
      {route === "billing" && <section id="hosted-billing-card"><h2 tabIndex={-1}>{copy("hosted_subscribe_headline")}</h2><p>{copy("production_billing_body")}</p><button className="primary-button" onClick={() => void bridge.hosted_open_billing()}>{copy("production_billing_open")}</button><button className="secondary-button" onClick={() => void refreshAccount()}>{copy("production_billing_check")}</button></section>}
      {route === "provisioning" && <section id="hosted-provisioning-card"><h2 tabIndex={-1}>{copy("provisioning_headline")}</h2><div role="progressbar" aria-label={copy("provisioning_headline")} /><p>{copy("setup_provisioning_body")}</p>{provisionError !== null && <p id="hosted-provision-error" role="alert">{copy("production_provision_retry")}</p>}{provisionError !== null && <p id="hosted-provision-error-code" className="setup-hint" data-error-code={provisionError}>Error code: {provisionError}</p>}{provisionError !== null && <button id="hosted-provision-retry-button" className="secondary-button" disabled={busy} onClick={startProvisioning}>{copy("production_provision_resume")}</button>}</section>}
      {route === "passphrase" && <section id="hosted-passphrase-card"><h2 tabIndex={-1}>{copy("hosted_create_vault_headline")}</h2><p>{copy("hosted_create_vault_body")}</p><button id="passphrase-native-cta-button" className="primary-button" disabled={busy} onClick={startProvisioning}>{copy(provisionError === null ? "production_create_vault" : "try_again")}</button>{provisionError !== null && <p id="hosted-passphrase-error" role="alert">{copy("hosted_create_vault_error")}</p>}{provisionError !== null && <p id="hosted-passphrase-error-code" className="setup-hint" data-error-code={provisionError}>Error code: {provisionError}</p>}</section>}
    </>
  );

  let content: ReactNode;
  if (community) {
    const headline = hostedBrowser ? copy("hosted_join_headline") : copy("self_hosted_headline");
    const body = fixedOrigin ? copy("setup_choice_existing_hint") : copy("setup_self_hosted_join_body");
    content = (
      <section id="self-hosted-panel" aria-label={headline}>
        {!enrolledWithoutPhone && !enrollmentEstablished && <><h2 tabIndex={-1}>{headline}</h2><p>{body}</p></>}
        {fixedOrigin && <p id="fixed-self-hosted-origin">{hostedBrowser && <span className="setup-hint">Server: </span>}{fixedOrigin}</p>}
        {showUrlForm && (
          <>
            <div id="self-hosted-url-field" className="setup-url-field">
              <label>Server URL<input id="self-hosted-url-input" type="url" value={selfHostedUrl} placeholder="https://server.example" aria-invalid={selfHostedUrlError ? "true" : undefined} aria-describedby={selfHostedUrlError ? "self-hosted-url-error self-hosted-url-hint" : "self-hosted-url-hint"} onChange={event => setSelfHostedUrl(event.target.value)} onBlur={() => setSelfHostedUrlBlurred(true)} /></label>
              {selfHostedUrlError && <span id="self-hosted-url-error" className="setup-field-error" aria-live="polite">{copy(selfHostedUrlError)}</span>}
              <span id="self-hosted-url-hint" className="setup-hint">{copy("setup_self_hosted_url_hint")}</span>
            </div>
            <button id="self-hosted-connect-button" className="primary-button" disabled={!validOrigin || busy} onClick={connectSelfHosted}>Connect</button>
          </>
        )}
        {(view !== null || fixedOrigin || enrolledWithoutPhone || enrollmentEstablished) && <div id="enrollment-panel">{renderEnrollment()}</div>}
        {hostedBrowser && <p id="setup-account-billing-hint" className="setup-hint"><a id="setup-account-billing-link" href={accountUrl} target="_blank" rel="noopener noreferrer">Manage account &amp; billing<span className="visually-hidden"> (opens in a new tab)</span></a></p>}
      </section>
    );
  } else {
    content = hostedPanels;
  }

  return (
    <section id="setup-landing" aria-label="Set up Peppy">
      <div id="setup-landing-inner">
        <header id="setup-landing-header">
          <h1>Set up Peppy</h1>
          {!enrolledWithoutPhone && !fixedOrigin && <select ref={modeSelect} id="setup-mode-select" aria-label="Server mode" disabled={busy && stage === "verify_phone"} value={mode} onChange={event => selectMode(event.target.value as Mode)}><option value="hosted">{copy("setup_mode_hosted")}</option><option value="self-hosted">{copy("setup_mode_self_hosted")}</option></select>}
        </header>
        <div id="setup-landing-body" role="region" ref={bodyRef}>
          {pendingOperation && <BusyIndicator id="setup-pending-operation" label={copy(pendingOperation.label)} />}
          {!pendingOperation && hostedOperation && <BusyIndicator id="setup-hosted-operation" label={copy(hostedOperation)} />}
          {!pendingOperation && !hostedOperation && ownerStatusChecking && <BusyIndicator id="setup-owner-status-checking" label={copy("production_pairing_checking")} />}
          {!pendingOperation && !hostedOperation && !ownerStatusChecking && connectionPending && <BusyIndicator id="setup-connection-checking" label={copy("production_pairing_checking")} />}
          {content}
        </div>
      </div>
      <div id="setup-status-region" className="visually-hidden" role="status" aria-live="polite" aria-atomic="true">{statusMessage}</div>
    </section>
  );
}
