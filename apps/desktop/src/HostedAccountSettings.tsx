import { useCallback, useEffect, useRef, useState } from "react";
import { AttachmentStorageSettings } from "@peppy/desktop-ui";
import { bridge, type AttachmentRetentionStatusResult, type HostedAccountView } from "./bridge";
import { peppyCopy } from "./generated/peppyCopy";
import { openPrivacyPolicy, PRIVACY_POLICY_URL } from "./PrivacySettings";

type Action = "sign-in" | "sign-out";

export function HostedAccountSettings({ sessionScopeId }: { sessionScopeId?: string }) {
  const [account, setAccount] = useState<HostedAccountView | null>(null);
  const [loading, setLoading] = useState(true);
  const [loadFailed, setLoadFailed] = useState(false);
  const [availabilityError, setAvailabilityError] = useState(false);
  const [actionError, setActionError] = useState<Action | null>(null);
  const [pending, setPending] = useState(false);
  const mounted = useRef(true);
  const loadSequence = useRef(0);
  const pendingRef = useRef(false);
  const focusAfterAction = useRef(false);
  const signInButton = useRef<HTMLButtonElement>(null);
  const signOutButton = useRef<HTMLButtonElement>(null);
  const statusHeading = useRef<HTMLHeadingElement>(null);

  const load = useCallback(async (signedOutOnFailure = false) => {
    if (!mounted.current) return false;
    const sequence = ++loadSequence.current;
    setLoading(true);
    setLoadFailed(false);
    setAvailabilityError(false);
    try {
      const next = await bridge.hosted_account();
      if (mounted.current && sequence === loadSequence.current) setAccount(next);
      return true;
    } catch {
      if (mounted.current && sequence === loadSequence.current) {
        if (signedOutOnFailure) {
          setAccount(current => current && { ...current, signedIn: false, available: false });
          setAvailabilityError(true);
        } else setLoadFailed(true);
      }
      return false;
    } finally {
      if (mounted.current && sequence === loadSequence.current) setLoading(false);
    }
  }, []);

  useEffect(() => {
    mounted.current = true;
    void load();
    return () => { mounted.current = false; };
  }, [load]);

  useEffect(() => {
    // An earlier render's queued effect can run after an action starts, before pending commits.
    if (!focusAfterAction.current || loading || pending || pendingRef.current) return;
    const nextButton = account?.signedIn ? signOutButton.current : signInButton.current;
    if (nextButton && !nextButton.disabled) nextButton.focus();
    else statusHeading.current?.focus();
    focusAfterAction.current = false;
  }, [account?.signedIn, loading, pending]);

  const runAction = async (action: Action) => {
    if (pendingRef.current || !account) return;
    pendingRef.current = true;
    focusAfterAction.current = true;
    setPending(true);
    setActionError(null);
    ++loadSequence.current;
    try {
      if (action === "sign-out") {
        await bridge.hosted_sign_out();
        if (mounted.current) setAccount(current => current && { ...current, signedIn: false, available: false });
        await load(true);
      } else {
        await bridge.hosted_sign_in("google");
        await load();
      }
    } catch {
      if (mounted.current) setActionError(action);
    } finally {
      pendingRef.current = false;
      if (mounted.current) setPending(false);
    }
  };

  return <div id="hosted-account-session" data-hosted-account-session className="hosted-account-session" aria-busy={loading || undefined}>
    <h3 id="hosted-account-session-status" ref={statusHeading} className="visually-hidden" tabIndex={-1}>Account session</h3>
    {loading && <p id="hosted-account-loading" role="status" className="settings-note">Account loading…</p>}
    {!loading && loadFailed && <>
      <p id="hosted-account-error" role="alert" className="settings-error">Could not load account status.</p>
      <button id="hosted-account-retry" type="button" className="secondary-button" onClick={() => void load()}>Retry</button>
    </>}
    {!loading && !loadFailed && account?.signedIn && <>
      {account.accountLabel !== null && <p id="hosted-account-label" className="settings-note">{account.accountLabel}</p>}
      <p id="hosted-account-sign-out-scope" className="settings-note">Signing out ends this app’s account session. Your vault connection and saved data remain.</p>
      <button id="hosted-account-sign-out" ref={signOutButton} type="button" className="secondary-button" aria-describedby="hosted-account-sign-out-scope" disabled={pending} aria-busy={pending || undefined} onClick={() => void runAction("sign-out")}>Sign out of Peppy</button>
      {actionError === "sign-out" && <p id="hosted-account-sign-out-error" role="alert" className="settings-error">Sign-out was not completed. Try again.</p>}
    </>}
    {!loading && !loadFailed && account && !account.signedIn && <>
      <button id="hosted-account-sign-in" ref={signInButton} type="button" className="secondary-button" disabled={pending || !account.available} aria-busy={pending || undefined} onClick={() => void runAction("sign-in")}>Sign in with Google</button>
      {!account.available && !availabilityError && <p id="hosted-account-unavailable" role="status" className="settings-note">Sign-in is not available right now.</p>}
      {availabilityError && <><p id="hosted-account-error" role="alert" className="settings-error">Could not load account status.</p><button id="hosted-account-retry" type="button" className="secondary-button" onClick={() => void load(true)}>Retry</button></>}
      {actionError === "sign-in" && <p id="hosted-account-sign-in-error" role="alert" className="settings-error">Sign-in was not completed. Try again.</p>}
    </>}
    <AttachmentStorageSettingsHost sessionScopeId={sessionScopeId} onManageStorage={bridge.hosted_open_billing} />
  </div>;
}


export type AttachmentStorageSettingsHostProps = {
  sessionScopeId?: string;
  onManageStorage?(): Promise<void> | void;
  manageStorageUrl?: string;
  hostKind?: "native" | "browser";
  privacyUrl?: string;
};

export function AttachmentStorageSettingsHost({ sessionScopeId, onManageStorage, manageStorageUrl, hostKind = "native", privacyUrl = PRIVACY_POLICY_URL }: AttachmentStorageSettingsHostProps) {
  const [storage, setStorage] = useState<AttachmentRetentionStatusResult | null>(null);
  const [storageScope, setStorageScope] = useState(sessionScopeId ?? "legacy-session");
  const [loading, setLoading] = useState(true);
  const [stale, setStale] = useState(false);
  const [acknowledgementError, setAcknowledgementError] = useState(false);
  const [acknowledgementRetryKey, setAcknowledgementRetryKey] = useState(0);
  const [manageError, setManageError] = useState(false);
  const [privacyError, setPrivacyError] = useState(false);
  const mounted = useRef(true);
  const loadSequence = useRef(0);
  const storageRef = useRef<AttachmentRetentionStatusResult | null>(null);
  const hostEpoch = useRef(0);
  const activeScope = useRef(sessionScopeId ?? "legacy-session");
  const currentScope = sessionScopeId ?? "legacy-session";
  if (activeScope.current !== currentScope) {
    activeScope.current = currentScope;
    hostEpoch.current += 1;
  }

  const currentHost = () => ({ epoch: hostEpoch.current, scope: activeScope.current });
  const isCurrentHost = (host: { epoch: number; scope: string }) => mounted.current && host.epoch === hostEpoch.current && host.scope === activeScope.current;

  const updateStorage = useCallback((next: AttachmentRetentionStatusResult, scope: string) => {
    storageRef.current = next;
    setStorageScope(scope);
    setStorage(next);
  }, []);

  const loadStorage = useCallback(async (host = currentHost()): Promise<boolean> => {
    if (!bridge.attachment_retention_status) return false;
    const sequence = ++loadSequence.current;
    setLoading(true);
    try {
      const next = await bridge.attachment_retention_status();
      if (!isCurrentHost(host) || sequence !== loadSequence.current) return false;
      if (next.kind === "temporarily_unavailable" && storageRef.current?.kind === "available") {
        setStale(true);
      } else {
        updateStorage(next, host.scope);
        setStale(false);
      }
      return next.kind === "available";
    } catch {
      if (!isCurrentHost(host) || sequence !== loadSequence.current) return false;
      if (storageRef.current?.kind === "available") setStale(true);
      else updateStorage({ kind: "temporarily_unavailable" }, host.scope);
      return false;
    } finally {
      if (isCurrentHost(host) && sequence === loadSequence.current) setLoading(false);
    }
  }, [updateStorage]);

  const acknowledgeStorageNotice = useCallback(async (scopeId: string, policyVersion: string): Promise<boolean> => {
    if (!bridge.acknowledge_attachment_retention_notice) return false;
    const host = currentHost();
    setAcknowledgementError(false);
    try {
      const result = await bridge.acknowledge_attachment_retention_notice({ scopeId, policyVersion });
      if (!isCurrentHost(host)) return false;
      if (result === "acknowledged") {
        await loadStorage(host);
        return isCurrentHost(host);
      }
      if (result === "stale_scope") await loadStorage(host);
      else setAcknowledgementError(true);
      return false;
    } catch {
      if (isCurrentHost(host)) setAcknowledgementError(true);
      return false;
    }
  }, [loadStorage]);

  useEffect(() => {
    const host = currentHost();
    mounted.current = true;
    storageRef.current = null;
    setStorage(null);
    setStale(false);
    setAcknowledgementError(false);
    setAcknowledgementRetryKey(0);
    setManageError(false);
    setPrivacyError(false);
    void loadStorage(host);
    return () => {
      mounted.current = false;
      ++loadSequence.current;
    };
  }, [loadStorage, sessionScopeId, updateStorage]);

  const openManageStorage = async () => {
    if (!onManageStorage) return;
    setManageError(false);
    try {
      await onManageStorage();
    } catch {
      setManageError(true);
    }
  };

  const openPrivacy = async () => {
    setPrivacyError(false);
    try {
      await openPrivacyPolicy();
    } catch {
      setPrivacyError(true);
    }
  };

  const showingCurrentStorage = storageScope === currentScope;

  return <>
    <AttachmentStorageSettings key={sessionScopeId ?? "legacy-session"}
    result={showingCurrentStorage ? storage : null}
    loading={!showingCurrentStorage || loading}
    stale={stale}
    acknowledgementError={acknowledgementError}
    acknowledgementRetryKey={acknowledgementRetryKey}
    copy={peppyCopy}
    onRefresh={() => { const host = currentHost(); void loadStorage(host).then((accepted) => { if (accepted && isCurrentHost(host)) setAcknowledgementRetryKey((value) => value + 1); }); }}
    onAcknowledge={acknowledgeStorageNotice}
    onManageStorage={onManageStorage ? openManageStorage : undefined}
    manageStorageUrl={manageStorageUrl}
    onOpenPrivacy={hostKind === "native" ? openPrivacy : undefined}
    manageStorageError={manageError}
    privacyError={privacyError}
    privacyUrl={privacyUrl}
    />
  </>;
}
