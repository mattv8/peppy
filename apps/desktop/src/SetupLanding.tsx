import { useEffect, useRef, useState, type ReactNode } from "react";
import QRCode from "qrcode";
import { bridge, type EnrollmentView, type HostedAccountView } from "./bridge";
import { peppyCopy, type PeppyCopyKey } from "./generated/peppyCopy";
import "./setup-landing.css";

type Mode = "hosted" | "self-hosted";
type HostedPath = "chooser" | "existing" | "new";
type Route = "checking" | "chooser" | "new" | "enrollment" | "lapsed" | "billing" | "provisioning" | "passphrase";
type Method = "transfer" | "shared_secret";
type Stage = EnrollmentView["stage"];

const copy = (key: PeppyCopyKey) => peppyCopy[key];
const pollingStages = new Set<Stage>(["waiting_phone", "verify_phone"]);
const pollIntervalMs = 4_000;
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

const enrollmentErrorMessage = (errorCode: EnrollmentView["errorCode"]) => {
  if (errorCode === "invalid_setup_token") return copy("enrollment_error_invalid_setup_token");
  if (errorCode === "instance_already_claimed") return copy("enrollment_error_already_claimed");
  if (errorCode === "invite_invalid") return copy("enrollment_error_invite_invalid");
  if (errorCode === "vault_owner_required") return copy("enrollment_error_vault_owner_required");
  if (errorCode === "username_taken") return "That username is already in use. Choose another.";
  if (errorCode === "invalid_username") return "Choose a valid username.";
  if (errorCode === "vault_request_completed") return "This vault request was already completed. Contact an owner if you need access.";
  if (errorCode === "request_completed_without_material") return "This vault request was already completed. Start a new request to try again.";
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

export function SetupLanding({ mode, onMode, enrolledWithoutPhone, addingPhone = false, onExit, onJoined, fixedOrigin, accountUrl }: {
  mode: Mode;
  onMode(mode: Mode): void;
  enrolledWithoutPhone: boolean;
  addingPhone?: boolean;
  onExit?(): void;
  onJoined(): void;
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
  const [verified, setVerified] = useState(false);
  const [busy, setBusy] = useState(false);
  const [acknowledged, setAcknowledged] = useState(false);
  const [provisionError, setProvisionError] = useState(false);
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
  const inviteLinkInput = useRef<HTMLInputElement>(null);
  const joined = useRef(false);
  const provisioning = useRef(false);
  const discoverStarted = useRef(false);
  const fixedStarted = useRef(false);
  const enrollEpoch = useRef(0);
  const invitePollEpoch = useRef(0);
  const accountFetchEpoch = useRef(0);
  const signInEpoch = useRef(0);
  const enrollmentActive = useRef(false);
  const stageRef = useRef<Stage | null>(null);
  const retryRef = useRef<() => void>(() => undefined);
  const previousMode = useRef(mode);
  const previousRoute = useRef<Route | null>(null);

  const community = Boolean(fixedOrigin) || mode === "self-hosted" || enrolledWithoutPhone || addingPhone;
  const hostedBrowser = Boolean(fixedOrigin && accountUrl);

  const apply = (next: EnrollmentView) => {
    enrollmentActive.current = pollingStages.has(next.stage);
    if (next.stage !== stageRef.current) setVerified(false);
    stageRef.current = next.stage;
    setView(next);
  };
  const failedView = (): EnrollmentView => ({ mode: mode === "hosted" ? "hosted" : "community", stage: "error", origin: connectedOrigin ?? undefined, errorCode: "unavailable" });
  const runEnrollment = async (action: () => Promise<EnrollmentView>, retry: () => void) => {
    const epoch = ++enrollEpoch.current;
    retryRef.current = retry;
    setBusy(true);
    try {
      const next = await action();
      if (epoch === enrollEpoch.current) apply(next);
    } catch {
      if (epoch === enrollEpoch.current) apply(failedView());
    } finally {
      if (epoch === enrollEpoch.current) setBusy(false);
    }
  };
  const cancelActive = () => {
    if (!enrollmentActive.current) return;
    enrollmentActive.current = false;
    void bridge.enrollment_cancel().catch(() => undefined);
  };
  const resetEnrollment = () => {
    enrollEpoch.current += 1;
    invitePollEpoch.current += 1;
    cancelActive();
    enrollmentActive.current = false;
    stageRef.current = null;
    setView(null);
    setQr("");
    setVerified(false);
    setBusy(false);
  };
  const refreshAccount = async () => {
    const epoch = ++accountFetchEpoch.current;
    setAccountError(false);
    setAccountLoading(true);
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
      if (epoch === accountFetchEpoch.current) setAccountLoading(false);
    }
  };

  const discover = (origin?: string) => runEnrollment(() => bridge.enrollment_discover(origin), () => discover(origin));
  const createOwner = () => runEnrollment(() => bridge.enrollment_create_owner({ username }), createOwner);
  const signInEnrollment = () => runEnrollment(() => bridge.enrollment_sign_in({ username }), signInEnrollment);
  const connectSharedSecret = () => runEnrollment(() => bridge.enrollment_connect_shared_secret(vaultOwner ? { vaultOwner } : undefined), connectSharedSecret);
  const beginVaultRequest = () => runEnrollment(async () => {
    setInviteError(null);
    try { return await bridge.enrollment_begin_vault_request({ inviteLink, displayName }); }
    catch (error) {
      if (typeof error === "object" && error !== null && "code" in error && (error as { code?: string }).code === "invite_invalid") {
        setInviteError(copy("enrollment_error_invite_invalid"));
        return { mode: "community", stage: "invite_request" };
      }
      throw error;
    }
  }, beginVaultRequest);
  const createVault = () => runEnrollment(() => bridge.enrollment_create_vault({ username: vaultUsername }), createVault);
  const addPhone = (next: Method) => {
    setMethod(next);
    void runEnrollment(() => bridge.enrollment_add_phone(next), () => addPhone(next));
  };
  const approve = () => {
    if (!verified) return;
    void runEnrollment(() => bridge.enrollment_approve(), approve);
  };
  const refreshStatus = () => runEnrollment(() => bridge.enrollment_status(), refreshStatus);
  const cancelPairing = () => {
    invitePollEpoch.current += 1;
    if (addingPhone) {
      enrollEpoch.current += 1;
      enrollmentActive.current = false;
      void bridge.enrollment_cancel().catch(() => undefined).finally(onExit);
      return;
    }
    void runEnrollment(async () => {
      await bridge.enrollment_cancel();
      enrollmentActive.current = false;
      return bridge.enrollment_status();
    }, cancelPairing);
  };
  const changeServer = () => {
    enrollEpoch.current += 1;
    cancelActive();
    setBusy(false);
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
      if (epoch === signInEpoch.current) setBusy(false);
    }
  };

  const invalidateAccountFlows = () => {
    accountFetchEpoch.current += 1;
    signInEpoch.current += 1;
  };

  useEffect(() => () => {
    invalidateAccountFlows();
    enrollEpoch.current += 1;
    cancelActive();
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
    cancelActive();
    void refreshStatus();
  }, [addingPhone, enrolledWithoutPhone]);

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
    if (addingPhone || route !== "enrollment" || discoverStarted.current) return;
    discoverStarted.current = true;
    void discover();
  }, [addingPhone, route, account?.signedIn]);
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
  useEffect(() => {
    if (stage !== "locked") setVaultOwner("");
    if (stage !== "signin") setSigninPath("chooser");
  }, [stage]);
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
    if (!payload) {
      setQr("");
      return;
    }
    let live = true;
    void QRCode.toDataURL(payload, qrOptions).then(url => { if (live) setQr(url); }).catch(() => { if (live) setQr(""); });
    return () => {
      live = false;
    };
  }, [payload]);
  useEffect(() => {
    if (!stage || !pollingStages.has(stage)) return;
    const timer = window.setInterval(() => {
      const epoch = enrollEpoch.current;
      void bridge.enrollment_status().then(next => {
        if (epoch === enrollEpoch.current) apply(next);
      }).catch(() => {
        if (epoch !== enrollEpoch.current) return;
        retryRef.current = refreshStatus;
        apply(failedView());
      });
    }, pollIntervalMs);
    return () => window.clearInterval(timer);
  }, [stage]);
  const joinedNow = stage === "phone_connected" || (!addingPhone && stage === "ready" && Boolean(view?.hasGateway));
  useEffect(() => {
    if (!joinedNow || joined.current) return;
    joined.current = true;
    onJoined();
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
    setProvisionError(false);
    setBusy(true);
    void bridge.hosted_provision().catch(() => setProvisionError(true)).finally(() => {
      setBusy(false);
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
            <div className="enrollment-actions"><button className="secondary-button" disabled={busy} onClick={() => void discover()}>{copy("try_again")}</button></div>
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
                <p>{copy("enrollment_setup_open")}</p>
                <p className="setup-hint">{nativeSetupPromptHint("open")}</p>
              </>
            ) : (
              <>
                <p>{copy("enrollment_setup_protected")}</p>
                <p className="setup-hint">{nativeSetupPromptHint("protected")}</p>
              </>
            )}
            <div id="enrollment-setup-username-field" className="setup-url-field"><label>{copy("enrollment_username_label")}<input id="enrollment-setup-username" type="text" autoComplete="username" value={username} onChange={event => setUsername(event.target.value)} aria-describedby="enrollment-setup-username-hint" /></label><span id="enrollment-setup-username-hint" className="setup-hint">{copy("enrollment_setup_username_hint")}</span></div>
            <div className="enrollment-actions">
              <button id="enrollment-create-owner-button" className="primary-button" disabled={busy} onClick={() => void createOwner()}>Create server</button>
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
            <fieldset className="enrollment-methods">
              <legend>How should the phone connect?</legend>
              <label><input type="radio" name="enrollment-method" value="transfer" checked={method === "transfer"} onChange={() => setMethod("transfer")} /> Transfer keys securely</label>
              <label><input type="radio" name="enrollment-method" value="shared_secret" checked={method === "shared_secret"} onChange={() => setMethod("shared_secret")} /> Enter shared secret on phone</label>
            </fieldset>
            <div className="enrollment-actions">
              <button id="enrollment-add-phone-button" className="primary-button" disabled={busy} onClick={() => addPhone(method)}>Add phone</button>
            </div>
          </Stage>
        );
      case "waiting_phone":
        return (
          <Stage stage="waiting_phone">
            <Heading>Connect your phone</Heading>
            <p>Open Peppy on your phone and scan this code.</p>
            {qr && (
              <div id="enrollment-qr-hero">
                <img id="enrollment-qr-image" src={qr} alt="" aria-hidden="true" />
                <span className="visually-hidden" role="img" aria-label={copy("production_pairing_qr_accessibility")} />
                {view.expiresInSeconds !== undefined && <span id="enrollment-countdown" role="timer" aria-live="off" aria-label={`${view.expiresInSeconds} seconds remaining`}>Expires in {view.expiresInSeconds}s</span>}
              </div>
            )}
            <p className="setup-hint">{methodHints[method]}</p>
            <p className="setup-hint">This QR connects your phone to this server. It does not grant any permissions by itself.</p>
            <div className="enrollment-actions">
              <button id="enrollment-cancel-button" className="secondary-button" disabled={busy} onClick={() => void cancelPairing()}>Cancel</button>
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
            <p id="enrollment-error" role="alert">{enrollmentErrorMessage(view.errorCode)}</p>
            <div className="enrollment-actions">
              <button id="enrollment-retry-button" className="primary-button" disabled={busy} onClick={() => retryRef.current()}>Retry</button>
            </div>
          </Stage>
        );
      default:
        return null;
    }
  };

  const statusMessage = stage === "waiting_phone" ? "Scan this code with your phone."
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
      {route === "provisioning" && <section id="hosted-provisioning-card"><h2 tabIndex={-1}>{copy("provisioning_headline")}</h2><div role="progressbar" aria-label={copy("provisioning_headline")} /><p>{copy("setup_provisioning_body")}</p>{provisionError && <p id="hosted-provision-error" role="alert">{copy("production_provision_retry")}</p>}{provisionError && <button id="hosted-provision-retry-button" className="secondary-button" disabled={busy} onClick={startProvisioning}>{copy("production_provision_resume")}</button>}</section>}
      {route === "passphrase" && <section id="hosted-passphrase-card"><h2 tabIndex={-1}>{copy("passphrase_create_headline")}</h2><p>{copy("passphrase_create_body")}</p><p className="setup-hint">{copy("passphrase_irrecoverable_warn")}</p><p className="setup-hint">{copy("passphrase_native_note")}</p><label className="settings-check"><input id="passphrase-ack" type="checkbox" checked={acknowledged} onChange={event => setAcknowledged(event.target.checked)} /> {copy("passphrase_ack_label")}</label><button id="passphrase-native-cta-button" className="primary-button" disabled={!acknowledged || busy} onClick={startProvisioning}>{copy("passphrase_native_cta")}</button>{provisionError && <p id="hosted-passphrase-error" role="alert">{copy("production_provision_retry")}</p>}</section>}
    </>
  );

  let content: ReactNode;
  if (community) {
    const headline = hostedBrowser ? copy("hosted_join_headline") : copy("self_hosted_headline");
    const body = fixedOrigin ? copy("setup_choice_existing_hint") : copy("setup_self_hosted_join_body");
    content = (
      <section id="self-hosted-panel" aria-label={headline}>
        {!enrolledWithoutPhone && <><h2 tabIndex={-1}>{headline}</h2><p>{body}</p></>}
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
        {(view !== null || fixedOrigin) && <div id="enrollment-panel">{renderEnrollment()}</div>}
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
        <div id="setup-landing-body" role="region" ref={bodyRef}>{content}</div>
      </div>
      <div id="setup-status-region" className="visually-hidden" role="status" aria-live="polite" aria-atomic="true">{statusMessage}</div>
    </section>
  );
}
