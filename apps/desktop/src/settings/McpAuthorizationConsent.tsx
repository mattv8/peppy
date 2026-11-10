import { useEffect, useRef, useState } from "react";
import { Modal } from "@peppy/desktop-ui";
import type { McpDeviceStatus, McpScope } from "@peppy/desktop-ui/bridge-contracts";
import { bridge } from "../bridge/bridge";
import { mcpErrorMessage, mcpServerErrorCode } from "./mcpDisplay";
import { consentScopeLabels } from "./mcpScopes";
import "./device-settings.css";

type McpAuthorizationConsentProps = {
  authorizationId: string;
  browserHost: boolean;
  sessionKey: string;
  focusFallback: { current: HTMLElement | null };
  onClose(): void;
};

const isExpiredAuthorization = (reason: unknown) => mcpServerErrorCode(reason) === "mcp_authorization_not_found";

export function McpAuthorizationConsent({ authorizationId, browserHost, sessionKey, focusFallback, onClose }: McpAuthorizationConsentProps) {
  const [request, setRequest] = useState<Awaited<ReturnType<typeof bridge.mcp_authorization>> | null>(null);
  const [status, setStatus] = useState<McpDeviceStatus | null>(null);
  const [selected, setSelected] = useState<McpScope[]>([]);
  const [error, setError] = useState("");
  const [pending, setPending] = useState(false);
  const [retryVersion, setRetryVersion] = useState(0);
  const [redirectUrl, setRedirectUrl] = useState<string | null>(null);
  const [expired, setExpired] = useState(false);
  const mounted = useRef(true);
  const generation = useRef(0);
  const opener = useRef<HTMLElement | null>(typeof document === "undefined" || document.activeElement === document.body || !(document.activeElement instanceof HTMLElement) ? null : document.activeElement);

  useEffect(() => {
    const requestGeneration = ++generation.current;
    mounted.current = true;
    setRequest(null); setStatus(null); setRedirectUrl(null); setError(""); setExpired(false); setPending(false);
    void Promise.all([bridge.mcp_authorization(authorizationId), bridge.mcp_status()]).then(([next, currentStatus]) => {
      if (!mounted.current || generation.current !== requestGeneration) return;
      const vaultId = currentStatus.vaultId;
      if (typeof vaultId !== "string" || !next.resource.startsWith(`${currentStatus.serverOrigin}/mcp`)) throw new Error("This authorization request no longer matches the authenticated vault context.");
      setRequest(next); setStatus(currentStatus);
      setSelected(next.requestedScopes);
    }).catch(reason => {
      if (mounted.current && generation.current === requestGeneration) {
        setExpired(isExpiredAuthorization(reason));
        setError(mcpErrorMessage(reason, "This authorization request is unavailable."));
      }
    });
    return () => { mounted.current = false; };
  }, [authorizationId, sessionKey, retryVersion]);

  const redirect = async (url: string) => {
    if (browserHost) window.location.assign(url);
    else if (bridge.open_mcp_oauth_redirect) await bridge.open_mcp_oauth_redirect(url);
    else throw new Error("Opening the validated redirect is unavailable.");
  };
  const decide = async (approve: boolean) => {
    if (!request || !status || pending) return;
    const decisionGeneration = generation.current;
    setPending(true);
    try {
      const decision = approve
        ? await bridge.mcp_approve_authorization(request.id, selected)
        : await bridge.mcp_deny_authorization(request.id);
      if (!mounted.current || generation.current !== decisionGeneration) return;
      setRedirectUrl(decision.redirectUri);
      await redirect(decision.redirectUri);
      if (mounted.current && generation.current === decisionGeneration) onClose();
    } catch (reason) {
      if (mounted.current && generation.current === decisionGeneration) {
        setExpired(isExpiredAuthorization(reason));
        setError(mcpErrorMessage(reason, approve ? "Couldn't approve this request. Try again." : "Couldn't deny this request. Try again."));
      }
    } finally {
      if (mounted.current && generation.current === decisionGeneration) setPending(false);
    }
  };
  const toggleScope = (scope: McpScope, checked: boolean) => setSelected(scopes => request?.requestedScopes.filter(item => checked ? scopes.includes(item) || item === scope : scopes.includes(item) && item !== scope) ?? scopes);

  const retryNavigation = async () => {
    if (!redirectUrl || pending) return;
    setPending(true);
    const navigationGeneration = generation.current;
    try { await redirect(redirectUrl); if (mounted.current && generation.current === navigationGeneration) onClose(); }
    catch (reason) { if (mounted.current && generation.current === navigationGeneration) setError(mcpErrorMessage(reason, "The validated redirect could not be opened.")); }
    finally { if (mounted.current && generation.current === navigationGeneration) setPending(false); }
  };
  const ready = request !== null && status !== null;
  const zeroScopes = ready && selected.length === 0;
  const state = redirectUrl ? "redirect" : expired ? "expired" : error ? "error" : ready ? "ready" : "loading";
  const close = <button id="mcp-authorization-close" type="button" className="secondary-button" onClick={onClose}>Close</button>;
  let actions = close;
  if (!expired && ready && !redirectUrl) {
    actions = <><button id="mcp-authorization-deny" type="button" className="secondary-button" disabled={pending} onClick={() => void decide(false)}>Deny access</button><button id="mcp-authorization-approve" type="button" className="primary-button" aria-describedby={zeroScopes ? "mcp-scope-required" : undefined} disabled={pending || zeroScopes} onClick={() => void decide(true)}>Approve access</button></>;
  } else if (!expired && ready && redirectUrl) {
    actions = <>{close}<button id="mcp-authorization-retry-redirect" type="button" className="primary-button" disabled={pending} onClick={() => void retryNavigation()}>Retry opening redirect</button></>;
  } else if (!expired && error) {
    actions = <>{close}<button id="mcp-authorization-retry" type="button" className="primary-button" onClick={() => { setError(""); setRetryVersion(version => version + 1); }}>Retry</button></>;
  }
  return <Modal id="mcp-authorization-consent" title="Authorize MCP client" busy={pending} opener={opener} focusFallback={focusFallback} onDismiss={() => !pending && onClose()} actions={actions}>
    <div id="mcp-authorization-body" data-mcp-authorization-state={state}>
      {!ready && !error && <p id="mcp-authorization-loading" role="status">Loading authorization request…</p>}
      {expired && <><p id="mcp-authorization-expired" role="alert">This authorization request expired or was already used.</p><p>Start the connection again in your AI client.</p></>}
      {!ready && error && !expired && <p id="mcp-authorization-error" role="alert">{error}</p>}
      {ready && !expired && <>
        {error && <p id="mcp-authorization-error" role="alert">{error}</p>}
        <p id="mcp-authorization-client"><strong>{request.clientName}</strong> requests access to vault {status.vaultId} on {status.serverOrigin}.</p>
        <small id="mcp-authorization-name-note" className="settings-note">Name provided by the requesting app.</small>
        <dl id="mcp-authorization-identity"><div><dt>Returns to</dt><dd><span data-mcp-redirect-origin>{request.redirectOrigin}</span>{request.redirectIsLoopback && <span data-mcp-redirect-local>on this computer</span>}</dd></div><div><dt>Resource</dt><dd>{request.resource}</dd></div></dl>
        <details id="mcp-authorization-client-id"><summary>Client ID</summary><code>{request.clientId}</code></details>
        <fieldset id="mcp-scope-fieldset"><legend>Allow</legend>{request.requestedScopes.map(scope => <label key={scope} className="settings-check"><input type="checkbox" checked={selected.includes(scope)} disabled={pending} onChange={event => toggleScope(scope, event.target.checked)} /><span>{consentScopeLabels[scope]}</span></label>)}</fieldset>
        {zeroScopes && <small id="mcp-scope-required" className="settings-note">Select at least one permission to approve.</small>}
        {request.requestedScopes.some(scope => scope !== "conversations:read") && <p id="mcp-authorization-send-note">Sending includes ordinary reaction text and sends messages without per-send prompts.</p>}
        <p id="mcp-authorization-recall-note">Anything the client copies outside Peppy can't be recalled.</p>
        <p id="mcp-authorization-confirm-prompt"><strong>Approve only if you started this connection.</strong></p>
      </>}
    </div>
  </Modal>;
}
