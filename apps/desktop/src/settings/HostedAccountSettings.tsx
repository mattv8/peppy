import { useCallback, useEffect, useRef, useState, type RefObject } from "react";
import { AttachmentStorageSettings } from "@peppy/desktop-ui";
import { bridge, type AttachmentRetentionStatusResult, type BillingSummaryView, type HostedAccountView, type PurchaseOptions, type StoreProduct } from "../bridge/bridge";
import { formatCopy, peppyCopy } from "../generated/peppyCopy";
import { formatDeletionDeadline } from "../notices/systemRibbonModel";
import { DeleteAccountDialog, type AccountDeletionResult } from "./DeleteAccountDialog";
import { FeedbackSettings } from "./FeedbackSettings";

type SignInProvider = "apple" | "google";

function isCancelled(error: unknown): boolean {
  return typeof error === "object" && error !== null && "code" in error && error.code === "cancelled";
}

function ProviderButtons({ providers, busy, pendingProvider, onSignIn, signInButton }: { providers: SignInProvider[]; busy: boolean; pendingProvider: SignInProvider | null; onSignIn(provider: SignInProvider): void; signInButton: RefObject<HTMLButtonElement | null> }) {
  return <div id="hosted-account-sign-in-options" role="group" aria-label={peppyCopy.hosted_sign_in_group_label}>
    {providers.map(provider => <button key={provider} ref={provider === "google" ? signInButton : undefined} id={`hosted-account-sign-in-${provider}`} className="secondary-button provider-button" type="button" data-provider={provider} disabled={busy} aria-busy={pendingProvider === provider || undefined} onClick={() => onSignIn(provider)}>{provider === "apple" ? peppyCopy.hosted_sign_in_apple : peppyCopy.hosted_sign_in_google}</button>)}
    <p id="hosted-account-sign-in-status" role="status">{pendingProvider ? peppyCopy.hosted_sign_in_waiting : ""}</p>
  </div>;
}

const emptyBilling: BillingSummaryView = { provider: "none", manageableIn: "none", planActive: false, storagePacks: 0, storageRenewsWithoutPlan: false, deletionPending: false };

type DeletionStatus = { executeAfter: string | null; cancellable: boolean };

function deletionPhase(deletion: DeletionStatus | null, pending: boolean): "none" | "scheduled" | "processing" {
  if (!deletion) return pending ? "scheduled" : "none";
  if (!deletion.executeAfter) return pending ? "processing" : "none";
  return deletion.cancellable ? "scheduled" : "processing";
}

export function HostedAccountSettings({ sessionScopeId, onAccountDeleted, onDeletionCanceled }: { sessionScopeId?: string; onAccountDeleted?(result: AccountDeletionResult): void; onDeletionCanceled?(): void }) {
  const [account, setAccount] = useState<HostedAccountView | null>(null);
  const [billing, setBilling] = useState<BillingSummaryView | null>(null);
  const [loading, setLoading] = useState(true);
  const [loadFailed, setLoadFailed] = useState(false);
  const [signInError, setSignInError] = useState(false);
  const [pendingProvider, setPendingProvider] = useState<SignInProvider | null>(null);
  const [signingOut, setSigningOut] = useState(false);
  const [signOutError, setSignOutError] = useState(false);
  const [deleteOpen, setDeleteOpen] = useState(false);
  const [deletion, setDeletion] = useState<DeletionStatus | null>(null);
  const [cancelBusy, setCancelBusy] = useState(false);
  const [cancelError, setCancelError] = useState("");
  const [announcement, setAnnouncement] = useState("");
  const cancelInFlight = useRef(false);
  const focusAfterCancel = useRef<"open" | "status" | null>(null);
  const deleteButton = useRef<HTMLButtonElement>(null);
  const signInButton = useRef<HTMLButtonElement>(null);
  const accountLoadSequence = useRef(0);
  const focusAfterSignOut = useRef(false);
  const load = useCallback(async (initial = false) => {
    const sequence = ++accountLoadSequence.current;
    if (initial) setLoading(true);
    setLoadFailed(false);
    try {
      const nextAccount = await bridge.hosted_account();
      if (sequence !== accountLoadSequence.current) return;
      setAccount(nextAccount);
      const nextBilling = await bridge.billing_summary().catch(() => emptyBilling);
      const nextDeletion = nextAccount.signedIn ? await bridge.hosted_deletion_status().catch(() => undefined) : null;
      if (sequence !== accountLoadSequence.current) return;
      setBilling(nextBilling);
      if (nextDeletion !== undefined) setDeletion(nextDeletion);
    } catch { if (sequence === accountLoadSequence.current) { setAccount(current => current?.signedIn ? { ...current, signedIn: false } : current); setLoadFailed(true); } }
    finally { if (sequence === accountLoadSequence.current) setLoading(false); }
  }, [sessionScopeId]);
  useEffect(() => { setDeletion(null); }, [sessionScopeId]);
  useEffect(() => { void load(true); }, [load]);
  useEffect(() => { if (focusAfterSignOut.current && !loading && account && !account.signedIn) { signInButton.current?.focus(); focusAfterSignOut.current = false; } }, [account, loading]);
  useEffect(() => {
    if (!focusAfterCancel.current) return;
    const target = document.getElementById(focusAfterCancel.current === "open" ? "hosted-account-delete-open" : "hosted-account-delete-status");
    if (target) { target.focus(); focusAfterCancel.current = null; }
  }, [deletion, billing]);
  useEffect(() => { const refresh = () => void load(); window.addEventListener("focus", refresh); return () => window.removeEventListener("focus", refresh); }, [load]);
  const signIn = async (provider: SignInProvider) => {
    setPendingProvider(provider); setSignInError(false);
      try { await bridge.hosted_sign_in(provider); await load(); }
    catch (error) { if (!isCancelled(error)) setSignInError(true); }
    finally { setPendingProvider(null); }
  };
  const signOut = async () => {
    setSigningOut(true); setSignOutError(false);
    try { focusAfterSignOut.current = true; await bridge.hosted_sign_out(); await load(); }
    catch { setSignOutError(true); }
    finally { setSigningOut(false); }
  };
  const providers = account?.providers ?? [];
  const cancelDeletion = async () => {
    if (cancelInFlight.current) return;
    cancelInFlight.current = true;
    setCancelBusy(true);
    setCancelError("");
    setAnnouncement("");
    try {
      await bridge.hosted_cancel_account_deletion();
      setAnnouncement(peppyCopy.account_delete_cancel_done);
      focusAfterCancel.current = "open";
      setDeletion({ executeAfter: null, cancellable: false });
      setBilling(current => ({ ...(current ?? emptyBilling), deletionPending: false }));
      onDeletionCanceled?.();
      void load();
    } catch (error) {
      const code = typeof error === "object" && error !== null && "code" in error ? error.code : undefined;
      if (code === "deletion-already-started") {
        focusAfterCancel.current = "status";
        setDeletion(current => ({ executeAfter: current?.executeAfter ?? null, cancellable: false }));
        setBilling(current => ({ ...(current ?? emptyBilling), deletionPending: true }));
        setCancelError(peppyCopy.account_delete_cancel_started);
        void load();
      } else if (code === "deletion-not-scheduled") {
        focusAfterCancel.current = "open";
        setAnnouncement(peppyCopy.account_delete_cancel_none);
        setDeletion({ executeAfter: null, cancellable: false });
        setBilling(current => ({ ...(current ?? emptyBilling), deletionPending: false }));
        void load();
      } else {
        setCancelError(code === "hosted-session-required" && typeof error === "object" && error !== null && "message" in error && typeof error.message === "string" ? error.message : peppyCopy.account_delete_cancel_error);
      }
    } finally {
      cancelInFlight.current = false;
      setCancelBusy(false);
    }
  };
  const phase = deletionPhase(deletion, billing?.deletionPending === true);
  useEffect(() => { setCancelError(current => phase === "processing" && current === peppyCopy.account_delete_cancel_started ? current : ""); }, [phase]);
  const deadline = deletion?.executeAfter ? Date.parse(deletion.executeAfter) : NaN;
  const formattedDeadline = Number.isFinite(deadline) ? formatDeletionDeadline(deadline / 1000) : null;
  return <div id="hosted-account-session" data-hosted-account-session className="hosted-account-session" aria-busy={loading || undefined}>
    <h3 id="hosted-account-session-status" className="visually-hidden" tabIndex={-1}>Account session</h3>
    {loading && account === null && <p id="hosted-account-loading" role="status" className="settings-note">Account loading…</p>}
    {!loading && loadFailed && <><p id="hosted-account-error" role="alert" className="settings-error">Could not load account status.</p><button id="hosted-account-retry" type="button" className="secondary-button" onClick={() => void load()}>Retry</button></>}
    {!loadFailed && account?.signedIn && <><div id="hosted-account-sign-out-row"><div className="hosted-account-row-copy">{account.accountLabel && <p id="hosted-account-label">{account.accountLabel}</p>}<p id="hosted-account-sign-out-scope" className="settings-note">Signing out ends this app’s account session. Your vault connection and saved data remain.</p></div><button id="hosted-account-sign-out" type="button" className="secondary-button" disabled={signingOut} aria-busy={signingOut || undefined} onClick={() => void signOut()}>Sign out of Peppy</button></div>{signOutError && <p id="hosted-account-sign-out-error" role="alert" className="settings-error">Sign-out was not completed. Try again.</p>}<AttachmentStorageSettingsHost sessionScopeId={sessionScopeId} onManageStorage={bridge.hosted_open_billing} />
      {phase === "none" && <FeedbackSettings browserHost={false} bridge={bridge} />}
      <div id="hosted-account-delete" data-account-delete data-deletion-phase={phase}><div className="hosted-account-row-copy">
        <p id="hosted-account-delete-announcer" className="visually-hidden" role="status">{announcement}</p>
        <h3 id="hosted-account-delete-title">{phase === "none" ? peppyCopy.settings_server_delete : phase === "scheduled" ? peppyCopy.account_delete_pending_title : peppyCopy.account_delete_processing_title}</h3>
        {phase === "none" ? <p>{peppyCopy.account_delete_section_body}</p> : <p id="hosted-account-delete-status" role="status" tabIndex={-1}>{phase === "processing" ? peppyCopy.account_delete_pending_status : formattedDeadline ? formatCopy(peppyCopy.account_delete_scheduled_status, formattedDeadline) : peppyCopy.account_delete_scheduled_status_nodate}</p>}
        {phase === "scheduled" && <><p id="hosted-account-delete-readonly-note">{peppyCopy.account_delete_scheduled_readonly}</p>{billing?.provider === "stripe" && <p id="hosted-account-delete-subscription-note">{peppyCopy.account_delete_scheduled_stripe}</p>}{billing?.provider === "apple" && <p id="hosted-account-delete-subscription-note">{peppyCopy.account_delete_apple_billing}</p>}{billing?.provider === "google_play" && <p id="hosted-account-delete-subscription-note">{peppyCopy.account_delete_google_play_billing}</p>}</>}
        {cancelError && <p id="hosted-account-delete-cancel-error" role="alert" className="settings-error">{cancelError}</p>}
      </div>{phase === "none" ? <button id="hosted-account-delete-open" ref={deleteButton} className="destructive-button" type="button" aria-haspopup="dialog" onClick={() => setDeleteOpen(true)}>{peppyCopy.settings_server_delete}</button> : phase === "scheduled" ? <button id="hosted-account-delete-cancel" className="secondary-button" type="button" aria-disabled={cancelBusy || undefined} aria-busy={cancelBusy || undefined} onClick={() => void cancelDeletion()}>{peppyCopy.account_delete_cancel_action}</button> : null}</div>
      {deleteOpen && <DeleteAccountDialog billing={billing ?? emptyBilling} opener={deleteButton} onDismiss={() => setDeleteOpen(false)} onDeleted={result => { setDeleteOpen(false); if (result.outcome === "pending") { setDeletion(result.cancellable === undefined ? null : { executeAfter: result.executeAfter ?? null, cancellable: result.cancellable }); setBilling(current => ({ ...(current ?? emptyBilling), deletionPending: true })); } onAccountDeleted?.(result); }} />}</>}
    {!loadFailed && account && !account.signedIn && <>{account.available && providers.length ? <ProviderButtons providers={providers} busy={pendingProvider !== null} pendingProvider={pendingProvider} onSignIn={provider => void signIn(provider)} signInButton={signInButton} /> : <p id="hosted-account-unavailable" role="status" className="settings-note">Sign-in is not available.</p>}{signInError && <p id="hosted-account-sign-in-error" role="alert" className="settings-error">{peppyCopy.production_signin_failed}</p>}</>}
  </div>;
}

export type AttachmentStorageSettingsHostProps = { sessionScopeId?: string; onManageStorage?(): Promise<void> | void; manageStorageUrl?: string; };

export function AttachmentStorageSettingsHost({ sessionScopeId, onManageStorage, manageStorageUrl }: AttachmentStorageSettingsHostProps) {
  const [storage, setStorage] = useState<AttachmentRetentionStatusResult | null>(null);
  const storageRef = useRef<AttachmentRetentionStatusResult | null>(null);
  const [loading, setLoading] = useState(true);
  const [stale, setStale] = useState(false);
  const [manageStorageError, setManageStorageError] = useState(false);
  const [purchaseOptions, setPurchaseOptions] = useState<PurchaseOptions | null>(null);
  const [billingSummary, setBillingSummary] = useState<BillingSummaryView>(emptyBilling);
  const [products, setProducts] = useState<StoreProduct[]>([]);
  const [purchaseState, setPurchaseState] = useState<"loading" | "ready" | "unavailable">("loading");
  const loadSequence = useRef(0);
  const hostEpoch = useRef(0);
  const load = useCallback(async () => {
    const sequence = ++loadSequence.current;
    const epoch = hostEpoch.current;
    setLoading(true);
    try {
      const next = await bridge.attachment_retention_status?.() ?? null;
      if (sequence !== loadSequence.current || epoch !== hostEpoch.current) return;
      if (next?.kind === "temporarily_unavailable" && storageRef.current?.kind === "available") setStale(true);
      else { storageRef.current = next; setStorage(next); setStale(false); }
    } catch {
      if (sequence === loadSequence.current && epoch === hostEpoch.current) {
        if (storageRef.current?.kind === "available") setStale(true);
        else { setStorage({ kind: "temporarily_unavailable" }); setStale(false); }
      }
    } finally { if (sequence === loadSequence.current) setLoading(false); }
    try {
      const options = await bridge.purchase_options();
      if (sequence !== loadSequence.current || epoch !== hostEpoch.current) return;
      let summary = emptyBilling;
      try { summary = await bridge.billing_summary(); } catch { }
      if (sequence !== loadSequence.current || epoch !== hostEpoch.current) return;
      setBillingSummary(summary);
      setPurchaseOptions(options);
      if (options.appStore) { const next = await bridge.store_products(); if (sequence === loadSequence.current && epoch === hostEpoch.current) { setProducts(next); setPurchaseState(next.length ? "ready" : "unavailable"); } } else setPurchaseState("ready");
    } catch { if (sequence === loadSequence.current && epoch === hostEpoch.current) setPurchaseState("unavailable"); }
  }, []);
  useEffect(() => {
    hostEpoch.current += 1;
    loadSequence.current += 1;
    storageRef.current = null;
    setStorage(null);
    setBillingSummary(emptyBilling);
    setProducts([]);
    setPurchaseOptions(null);
    setPurchaseState("loading");
    setStale(false);
    setManageStorageError(false);
    void load();
  }, [load, sessionScopeId]);
  const showWebsiteManage = purchaseOptions?.website === true && billingSummary.provider !== "apple" && billingSummary.provider !== "google_play";
  const purchase = purchaseOptions ? { purchaseOptions, billingSummary, products, currentPacks: billingSummary.storagePacks, state: purchaseState, onPurchase: bridge.store_purchase, onPurchased: () => void load(), onManageAppStore: bridge.open_store_subscriptions, onManageGooglePlay: bridge.open_play_subscriptions, onManageWebsite: purchaseOptions.website ? onManageStorage : undefined, onRetry: () => void load() } : undefined;
  const manage = async () => { try { setManageStorageError(false); await onManageStorage?.(); } catch { setManageStorageError(true); } };
  return <AttachmentStorageSettings key={sessionScopeId ?? "legacy-session"} result={storage} loading={loading} stale={stale} copy={peppyCopy} onRefresh={() => void load()} onManageStorage={showWebsiteManage ? manage : undefined} manageStorageUrl={showWebsiteManage ? manageStorageUrl : undefined} manageStorageError={manageStorageError} purchase={purchase} />;
}
