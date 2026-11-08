import { useEffect, useRef } from "react";
import type { McpGrantView } from "@peppy/desktop-ui/bridge-contracts";
import { grantScopeLabels } from "./mcpScopes";
import { grantShortId } from "./mcpDisplay";

type McpGrantListProps = {
  grants: McpGrantView[];
  revokeTargetId: string | null;
  revoking: boolean;
  revokingGrantId: string | null;
  onStartRevoke(grant: McpGrantView): void;
  onCancelRevoke(): void;
  onConfirmRevoke(): void;
};

export function McpGrantList({ grants, revokeTargetId, revoking, revokingGrantId, onStartRevoke, onCancelRevoke, onConfirmRevoke }: McpGrantListProps) {
  const revokeOpeners = useRef(new Map<string, HTMLButtonElement>());
  const cancelButton = useRef<HTMLButtonElement>(null);
  const restoreFocusId = useRef<string | null>(null);
  const confirmButton = useRef<HTMLButtonElement>(null);
  const wasRevoking = useRef(false);

  useEffect(() => {
    if (revokeTargetId) {
      cancelButton.current?.focus();
      return;
    }
    if (restoreFocusId.current) {
      revokeOpeners.current.get(restoreFocusId.current)?.focus();
      restoreFocusId.current = null;
    }
  }, [revokeTargetId]);

  useEffect(() => {
    if (wasRevoking.current && !revoking && revokeTargetId) confirmButton.current?.focus();
    wasRevoking.current = revoking;
  }, [revoking, revokeTargetId]);

  const cancel = () => {
    restoreFocusId.current = revokeTargetId;
    onCancelRevoke();
  };

  return (
    <ul id="mcp-grant-list" aria-label="Authorized MCP clients">
      {grants.map(grant => {
        const shortId = grantShortId(grant);
        const confirming = revokeTargetId === grant.id;
        const confirmTextId = `mcp-grant-confirm-text-${grant.id}`;
        return (
          <li id={`mcp-grant-${grant.id}`} key={grant.id} data-mcp-grant={grant.id} aria-busy={revokingGrantId === grant.id || undefined}>
            <div data-mcp-grant-info>
              <strong>{grant.clientName}</strong>
              <dl data-mcp-grant-meta>
                <div>
                  <dt>Permissions</dt>
                  <dd>
                    <ul data-mcp-grant-scopes>
                      {grant.scopes.map(scope => <li key={scope}>{grantScopeLabels[scope]}</li>)}
                    </ul>
                  </dd>
                </div>
                <div><dt>Created</dt><dd>{grant.createdAt}</dd></div>
                <div><dt>Last used</dt><dd>{grant.lastUsedAt ?? "Never used"}</dd></div>
                <div><dt>ID</dt><dd><code>{shortId}</code></dd></div>
              </dl>
            </div>
            {confirming ? (
              <div id={`mcp-grant-confirm-${grant.id}`} data-mcp-grant-confirm={grant.id}>
                <p id={confirmTextId}>Stops new access for this authorization. Existing external copies remain.</p>
                <button id={`mcp-grant-cancel-${grant.id}`} ref={cancelButton} type="button" className="secondary-button" disabled={revoking} onClick={cancel}>Cancel</button>
                <button id={`mcp-grant-confirm-revoke-${grant.id}`} ref={confirmButton} type="button" className="primary-button" aria-describedby={confirmTextId} disabled={revoking} onClick={onConfirmRevoke}>
                  {`Confirm revoke ${grant.clientName} (${shortId})`}
                </button>
              </div>
            ) : (
              <button
                ref={element => {
                  if (element) revokeOpeners.current.set(grant.id, element);
                  else revokeOpeners.current.delete(grant.id);
                }}
                type="button"
                id={`mcp-grant-revoke-${grant.id}`}
                className="secondary-button"
                aria-label={`Revoke ${grant.clientName} (${shortId})`}
                disabled={revoking}
                onClick={() => onStartRevoke(grant)}
              >
                Revoke
              </button>
            )}
          </li>
        );
      })}
    </ul>
  );
}
