import { useCallback, useEffect, useRef, useState } from "react";
import { bridge, type HostedAccountView } from "./bridge";

type Action = "sign-in" | "sign-out";

export function HostedAccountSettings() {
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
  </div>;
}
