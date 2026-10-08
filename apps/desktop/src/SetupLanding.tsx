import { useEffect, useRef, useState, type ReactNode } from "react";
import QRCode from "qrcode";
import { bridge, type HostedAccountView, type JoinView } from "./bridge";
import { peppyCopy, type PeppyCopyKey } from "./generated/peppyCopy";
import "./setup-landing.css";

type Mode = "hosted" | "self-hosted";
type HostedPath = "chooser" | "existing" | "new";
type Route = "checking" | "chooser" | "existing" | "new" | "join" | "lapsed" | "billing" | "provisioning" | "passphrase";

const copy = (key: PeppyCopyKey) => peppyCopy[key];
const joinStates = new Set<JoinView["state"]>(["waiting", "claimed", "confirm"]);
const qrOptions = { errorCorrectionLevel: "M" as const, margin: 1, width: 256 };

export const savedSetupMode = (): Mode => window.localStorage.getItem("peppy.setup.mode") === "self-hosted" ? "self-hosted" : "hosted";

export function SetupLanding({ mode, onMode, enrolledWithoutPhone, pairPhone, selfHostedFallback, onJoined, fixedOrigin, accountUrl }: {
  mode: Mode;
  onMode(mode: Mode): void;
  enrolledWithoutPhone: boolean;
  pairPhone: ReactNode;
  selfHostedFallback: ReactNode;
  onJoined(): void;
  fixedOrigin?: string;
  accountUrl?: string;
}) {
  const [account, setAccount] = useState<HostedAccountView | null>(null);
  const [accountLoading, setAccountLoading] = useState(!fixedOrigin && mode === "hosted" && !enrolledWithoutPhone);
  const [accountError, setAccountError] = useState(false);
  const [signInError, setSignInError] = useState(false);
  const [hostedPath, setHostedPath] = useState<HostedPath>("chooser");
  const [join, setJoin] = useState<JoinView>({ state: "idle" });
  const [qr, setQr] = useState("");
  const [selfHostedUrl, setSelfHostedUrl] = useState(fixedOrigin ?? "");
  const [selfHostedOrigin, setSelfHostedOrigin] = useState<string | null>(fixedOrigin ?? null);
  const [acknowledged, setAcknowledged] = useState(false);
  const [verified, setVerified] = useState(false);
  const [busy, setBusy] = useState(false);
  const [provisionError, setProvisionError] = useState(false);
  const heading = useRef<HTMLHeadingElement>(null);
  const choice = useRef<HTMLButtonElement>(null);
  const modeSelect = useRef<HTMLSelectElement>(null);
  const confirmCheckbox = useRef<HTMLInputElement>(null);
  const joined = useRef(false);
  const provisioning = useRef(false);
  const autoJoinStarted = useRef(false);
  const joinStarting = useRef(false);
  const joinActive = useRef(false);
  const joinEpoch = useRef(0);
  const accountFetchEpoch = useRef(0);
  const signInEpoch = useRef(0);
  const previousMode = useRef(mode);
  const previousRoute = useRef<Route | null>(null);

  const invalidateJoin = () => { joinEpoch.current += 1; };
  const resetJoin = (cancel = true) => {
    invalidateJoin();
    if (cancel && joinActive.current) void bridge.join_cancel();
    joinActive.current = false;
    joinStarting.current = false;
    setJoin({ state: "idle" });
    setQr("");
    setVerified(false);
    setBusy(false);
  };
  const beginJoin = async (origin: string | null) => {
    if (joinStarting.current) return;
    joinStarting.current = true;
    joinActive.current = true;
    const epoch = ++joinEpoch.current;
    setJoin({ state: "idle" });
    setBusy(true);
    setVerified(false);
    setQr("");
    try {
      const next = await bridge.join_start(origin);
      if (epoch !== joinEpoch.current) return;
      setJoin(next);
      if (next.qrPayload) {
        const nextQr = await QRCode.toDataURL(next.qrPayload, qrOptions);
        if (epoch === joinEpoch.current) setQr(nextQr);
      }
    } catch {
      if (epoch === joinEpoch.current) setJoin({ state: "failed" });
    } finally {
      if (epoch === joinEpoch.current) {
        joinStarting.current = false;
        setBusy(false);
      }
    }
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

  useEffect(() => () => {
    accountFetchEpoch.current += 1;
    signInEpoch.current += 1;
    invalidateJoin();
    if (joinActive.current) void bridge.join_cancel();
  }, []);
  useEffect(() => {
    if (fixedOrigin || previousMode.current === mode) return;
    previousMode.current = mode;
    accountFetchEpoch.current += 1;
    signInEpoch.current += 1;
    resetJoin();
    setHostedPath("chooser");
    setAccount(null);
    setAccountError(false);
    setAccountLoading(mode === "hosted");
    setSelfHostedOrigin(null);
  }, [fixedOrigin, mode]);
  useEffect(() => {
    if (fixedOrigin || mode !== "hosted" || enrolledWithoutPhone) return;
    void refreshAccount();
    return () => {
      accountFetchEpoch.current += 1;
    };
  }, [fixedOrigin, mode, enrolledWithoutPhone]);

  const route: Route = fixedOrigin || mode === "self-hosted" ? "existing"
    : accountLoading && account === null ? "checking"
      : account?.signedIn
      ? account.hasVault && account.resumable ? "provisioning"
        : account.classification === "lapsed" ? "lapsed"
          : account.hasVault ? "join"
            : account.access !== "read_write" ? "billing"
              : account.resumable || account.classification === "provisioning" ? "provisioning"
                : "passphrase"
      : hostedPath === "existing" ? "join" : hostedPath;

  useEffect(() => {
    if (route !== "join" || !account?.signedIn || autoJoinStarted.current) return;
    autoJoinStarted.current = true;
    void beginJoin(null);
  }, [route, account?.signedIn]);
  useEffect(() => {
    if (!fixedOrigin || autoJoinStarted.current) return;
    autoJoinStarted.current = true;
    void bridge.join_status().then(async next => {
      if (next.state === "idle") {
        await beginJoin(fixedOrigin);
        return;
      }
      setJoin(next);
      if (next.qrPayload) setQr(await QRCode.toDataURL(next.qrPayload, qrOptions));
    }).catch(() => void beginJoin(fixedOrigin));
  }, [fixedOrigin]);
  useEffect(() => {
    if (route === "join") return;
    autoJoinStarted.current = false;
  }, [route]);
  useEffect(() => {
    const leftJoinRoute = previousRoute.current === "join" && route !== "join";
    previousRoute.current = route;
    if (leftJoinRoute && joinActive.current) resetJoin();
  }, [route]);
  useEffect(() => {
    if (enrolledWithoutPhone && joinActive.current) resetJoin();
  }, [enrolledWithoutPhone]);

  useEffect(() => {
    if (!joinStates.has(join.state)) return;
    const epoch = joinEpoch.current;
    const timer = window.setInterval(() => {
      void bridge.join_status().then(async next => {
        if (epoch !== joinEpoch.current) return;
        setJoin(previous => next.errorCode === "join-retrying" && next.expiresInSeconds === undefined ? { ...next, expiresInSeconds: previous.expiresInSeconds } : next);
        if (next.qrPayload) {
          const nextQr = await QRCode.toDataURL(next.qrPayload, qrOptions);
          if (epoch === joinEpoch.current) setQr(nextQr);
        }
      }).catch(() => {
        if (epoch === joinEpoch.current) setJoin({ state: "failed" });
      });
    }, 4_000);
    return () => window.clearInterval(timer);
  }, [join.state]);

  useEffect(() => {
    if (join.state !== "approved" || joined.current) return;
    joined.current = true;
    onJoined();
  }, [join.state, onJoined]);

  const provision = () => {
    setProvisionError(false);
    setBusy(true);
    void bridge.hosted_provision().catch(() => setProvisionError(true)).finally(() => {
      setBusy(false);
      void refreshAccount();
    });
  };
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
  useEffect(() => {
    if (mode === "hosted" && route === "chooser") {
      choice.current?.focus();
      return;
    }
    requestAnimationFrame(() => {
      if (join.state === "confirm") confirmCheckbox.current?.focus();
      else if (!fixedOrigin && mode === "self-hosted" && !selfHostedOrigin) modeSelect.current?.focus();
      else heading.current?.focus();
    });
  }, [fixedOrigin, mode, route, join.state, selfHostedOrigin]);

  const recordModeChoice = (next: Mode) => {
    if (!fixedOrigin) window.localStorage.setItem("peppy.setup.mode", next);
  };
  const selectMode = (next: Mode) => {
    recordModeChoice(next);
    if (next !== mode) {
      resetJoin();
      setHostedPath("chooser");
      setSelfHostedOrigin(null);
    }
    onMode(next);
  };
  const chooseExisting = () => {
    recordModeChoice("hosted");
    setHostedPath("existing");
    void beginJoin(null);
  };
  const chooseNew = () => {
    recordModeChoice("hosted");
    setHostedPath("new");
  };
  const backToChooser = () => {
    signInEpoch.current += 1;
    setSignInError(false);
    resetJoin();
    setHostedPath("chooser");
  };
  const cancelJoin = () => {
    resetJoin();
    if (!account?.signedIn && mode === "hosted") setHostedPath("chooser");
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
  const startProvisioning = () => {
    recordModeChoice("hosted");
    provision();
  };
  const connectSelfHosted = () => {
    recordModeChoice("self-hosted");
    setSelfHostedOrigin(validOrigin);
    void beginJoin(validOrigin);
  };
  const confirmJoin = async () => {
    if (!verified) return;
    const epoch = joinEpoch.current;
    setBusy(true);
    try {
      const next = await bridge.join_confirm();
      if (epoch === joinEpoch.current) setJoin(next);
    } catch {
      if (epoch === joinEpoch.current) setJoin({ state: "failed" });
    } finally {
      if (epoch === joinEpoch.current) setBusy(false);
    }
  };
  const validOrigin = fixedOrigin ?? (() => { try { const url = new URL(selfHostedUrl); return url.protocol === "https:" ? url.origin : null; } catch { return null; } })();

  const joinPanel = (
    <section id="hosted-join-panel" data-join-state={join.state} aria-label={copy("hosted_join_headline")}>
      <h2 ref={heading} tabIndex={-1}>{copy("hosted_join_headline")}</h2>
      {qr && joinStates.has(join.state) && <div id="join-qr-hero"><img id="join-qr-image" src={qr} alt="" aria-hidden="true" /><span id="join-qr-sr-label" className="visually-hidden" role="img" aria-label={copy("production_pairing_qr_accessibility")} />{join.expiresInSeconds !== undefined && <span id="join-countdown" role="timer" aria-live="off" aria-label={`${join.expiresInSeconds} seconds remaining`}>{join.expiresInSeconds}s</span>}</div>}
      {account?.signedIn && <p className="setup-hint">{copy("production_existing_vault")}</p>}
      {join.state === "waiting" && <><p id="join-state-message">{copy("setup_join_waiting")}</p><p className="setup-hint">{copy("setup_join_scan_hint")}</p></>}
      {join.state === "claimed" && <><p id="join-state-message">{copy("setup_join_claimed")}</p><Sas value={join.sas} /></>}
      {join.state === "confirm" && <div id="join-confirm-panel" data-panel="sas"><Sas value={join.sas} /><p>{copy("setup_join_confirm")}</p><label id="join-sas-confirm-label"><input ref={confirmCheckbox} id="join-sas-checkbox" type="checkbox" checked={verified} onChange={event => setVerified(event.target.checked)} /> {copy("production_pairing_verify")}</label><div id="join-confirm-actions"><button id="join-confirm-button" className="primary-button" disabled={!verified || busy} onClick={() => void confirmJoin()}>{copy("setup_join_continue")}</button><button id="join-cancel-button-confirm" className="secondary-button" disabled={busy} onClick={cancelJoin}>{copy("cancel")}</button></div></div>}
      {join.state === "approved" && <p id="join-state-message">{copy("setup_join_approved")}</p>}
      {join.state === "expired" && <p id="join-state-message">{copy("production_pairing_expired")}</p>}
      {join.state === "denied" && <p id="join-state-message">{copy("hosted_join_denied")}</p>}
      {join.state === "failed" && <p id="join-state-message">{copy("production_pairing_error")} {join.errorCode && <span>{join.errorCode}</span>}</p>}
      {join.errorCode === "join-retrying" && <p className="join-retrying-hint">{copy("setup_join_retrying")}</p>}
      <div id="join-actions">
        {(join.state === "waiting" || join.state === "claimed") && <button id="join-cancel-button" className="secondary-button" onClick={cancelJoin}>{copy("cancel")}</button>}
        {["expired", "denied", "failed"].includes(join.state) && <button id="join-refresh-button" className="secondary-button" disabled={busy} onClick={() => void beginJoin(fixedOrigin ?? (mode === "hosted" ? null : selfHostedOrigin))}>{copy("try_again")}</button>}
        {account?.signedIn && join.state === "idle" && <button id="join-refresh-button" className="secondary-button" disabled={busy} onClick={() => void beginJoin(null)}>{copy("try_again")}</button>}
        {!account?.signedIn && mode === "hosted" && <button id="setup-back-button" className="secondary-button" disabled={busy && join.state === "confirm"} onClick={backToChooser}>{copy("back")}</button>}
      </div>
    </section>
  );

  let content: ReactNode;
  if (fixedOrigin || mode === "self-hosted") {
    const hostedBrowser = Boolean(fixedOrigin && accountUrl);
    const headline = hostedBrowser ? copy("hosted_join_headline") : copy("self_hosted_headline");
    const body = fixedOrigin ? copy("setup_choice_existing_hint") : copy("setup_self_hosted_join_body");
    content = <section id="self-hosted-panel" aria-label={headline}><h2 ref={heading} tabIndex={-1}>{headline}</h2><p>{body}</p>{fixedOrigin ? <p id="fixed-self-hosted-origin">{hostedBrowser && <span className="setup-hint">Server: </span>}{fixedOrigin}</p> : <><label>Server URL<input id="self-hosted-url-input" type="url" value={selfHostedUrl} placeholder="https://server.example" aria-describedby="self-hosted-url-hint" onChange={event => setSelfHostedUrl(event.target.value)} /></label><span id="self-hosted-url-hint" className="setup-hint">{copy("setup_self_hosted_url_hint")}</span><button id="self-hosted-connect-button" className="primary-button" disabled={!validOrigin || busy} onClick={connectSelfHosted}>Connect</button></>}{(selfHostedOrigin || fixedOrigin) && joinPanel}{hostedBrowser && <p id="setup-account-billing-hint" className="setup-hint"><a id="setup-account-billing-link" href={accountUrl} target="_blank" rel="noopener noreferrer">Manage account &amp; billing<span className="visually-hidden"> (opens in a new tab)</span></a></p>}<details id="self-hosted-advanced"><summary>{copy("setup_self_hosted_advanced")}</summary>{selfHostedFallback}</details></section>;
  } else if (route === "checking") {
    content = <section id="hosted-account-checking" aria-busy="true"><h2 ref={heading} tabIndex={-1}>{copy("hosted_account_checking")}</h2></section>;
  } else if (route === "chooser") {
    content = <div id="hosted-path-chooser">{accountError && <p id="hosted-account-check-error" role="alert">{copy("hosted_account_check_failed")} <button className="secondary-button" onClick={() => void refreshAccount()}>{copy("try_again")}</button></p>}<div className="chooser-buttons"><button ref={choice} id="hosted-choice-existing" className="chooser-button" onClick={chooseExisting}><span className="chooser-button-label">{copy("setup_choice_existing")}</span><span className="chooser-button-hint">{copy("setup_choice_existing_hint")}</span></button><button id="hosted-choice-new" className="chooser-button" onClick={chooseNew}><span className="chooser-button-label">{copy("setup_choice_new")}</span><span className="chooser-button-hint">{copy("setup_choice_new_hint")}</span></button></div></div>;
  } else if (route === "new") {
    content = <section id="hosted-signin-panel" data-section="signin"><h2 ref={heading} tabIndex={-1}>{copy("hosted_sign_in_headline")}</h2><p>{copy("hosted_sign_in_body")}</p><button id="hosted-signin-google-button" className="primary-button" disabled={!account?.available || busy} onClick={() => void signIn()}>{copy("hosted_sign_in_google")}</button>{account && !account.available && <p id="hosted-signin-unavailable" role="alert">{copy("production_hosted_unavailable")}</p>}{accountError && <p id="hosted-account-check-error" role="alert">{copy("hosted_account_check_failed")} <button className="secondary-button" onClick={() => void refreshAccount()}>{copy("try_again")}</button></p>}{signInError && <p id="hosted-signin-error" role="alert">{copy("production_signin_failed")} <button className="secondary-button" onClick={() => void signIn()}>{copy("try_again")}</button></p>}<button id="setup-back-button" className="secondary-button" disabled={busy && join.state === "confirm"} onClick={backToChooser}>{copy("back")}</button></section>;
  } else if (route === "join") content = joinPanel;
  else if (route === "lapsed") content = <section id="hosted-lapsed-card"><h2 ref={heading} tabIndex={-1}>{copy("hosted_lapsed_headline")}</h2><p>{copy("hosted_lapsed_body")}</p><button className="primary-button" onClick={() => void bridge.hosted_open_billing()}>{copy("production_billing_open")}</button><button className="secondary-button" onClick={() => void refreshAccount()}>{copy("production_billing_check")}</button><button className="secondary-button" onClick={() => void bridge.hosted_sign_out()}>{copy("settings_server_sign_out")}</button></section>;
  else if (route === "billing") content = <section id="hosted-billing-card"><h2 ref={heading} tabIndex={-1}>{copy("hosted_subscribe_headline")}</h2><p>{copy("production_billing_body")}</p><button className="primary-button" onClick={() => void bridge.hosted_open_billing()}>{copy("production_billing_open")}</button><button className="secondary-button" onClick={() => void refreshAccount()}>{copy("production_billing_check")}</button></section>;
  else if (route === "provisioning") content = <section id="hosted-provisioning-card"><h2 ref={heading} tabIndex={-1}>{copy("provisioning_headline")}</h2><div role="progressbar" aria-label={copy("provisioning_headline")} /><p>{copy("setup_provisioning_body")}</p>{provisionError && <p id="hosted-provision-error" role="alert">{copy("production_provision_retry")}</p>}{provisionError && <button id="hosted-provision-retry-button" className="secondary-button" disabled={busy} onClick={startProvisioning}>{copy("production_provision_resume")}</button>}</section>;
  else content = <section id="hosted-passphrase-card"><h2 ref={heading} tabIndex={-1}>{copy("passphrase_create_headline")}</h2><p>{copy("passphrase_create_body")}</p><p className="setup-hint">{copy("passphrase_irrecoverable_warn")}</p><p className="setup-hint">{copy("passphrase_native_note")}</p><label className="settings-check"><input id="passphrase-ack" type="checkbox" checked={acknowledged} onChange={event => setAcknowledged(event.target.checked)} /> {copy("passphrase_ack_label")}</label><button id="passphrase-native-cta-button" className="primary-button" disabled={!acknowledged || busy} onClick={startProvisioning}>{copy("passphrase_native_cta")}</button>{provisionError && <p id="hosted-passphrase-error" role="alert">{copy("production_provision_retry")}</p>}</section>;

  if (enrolledWithoutPhone) content = pairPhone;
  return <section id="setup-landing" aria-label="Set up Peppy"><div id="setup-landing-inner"><header id="setup-landing-header"><h1>Set up Peppy</h1>{!enrolledWithoutPhone && !fixedOrigin && <select ref={modeSelect} id="setup-mode-select" aria-label="Server mode" disabled={busy && join.state === "confirm"} value={mode} onChange={event => selectMode(event.target.value as Mode)}><option value="hosted">{copy("setup_mode_hosted")}</option><option value="self-hosted">{copy("setup_mode_self_hosted")}</option></select>}</header><div id="setup-landing-body" role="region">{content}</div></div><div id="setup-status-region" className="visually-hidden" role="status" aria-live="polite" aria-atomic="true">{join.state === "waiting" ? copy("setup_join_waiting") : join.state === "claimed" ? copy("setup_join_claimed") : join.state === "confirm" ? copy("setup_join_confirm") : join.state === "approved" ? copy("setup_join_approved") : join.state === "expired" ? copy("production_pairing_expired") : join.state === "denied" ? copy("hosted_join_denied") : join.state === "failed" ? copy("production_pairing_error") : ""}</div></section>;
}

function Sas({ value }: { value?: string }) {
  return <div id="join-sas-panel" data-panel="sas"><output id="join-sas-code" aria-label={`Verification code: ${value ?? ""}`}>{value}</output></div>;
}
