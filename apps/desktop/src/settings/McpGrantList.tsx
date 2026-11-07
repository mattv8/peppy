import { useEffect, useRef } from "react";
import type { McpGrantView } from "@peppy/desktop-ui/bridge-contracts";
import { grantScopeLabels } from "./mcpScopes";
import { grantShortId } from "./mcpDisplay";

type McpGrantListProps = {
  grants: McpGrantView[];
  revokeTargetId: string | null;
  revoking: boolean;
  onStartRevoke(grant: McpGrantView): void;
  onCancelRevoke(): void;
  onConfirmRevoke(): void;
};

export function McpGrantList({ grants, revokeTargetId, revoking, onStartRevoke, onCancelRevoke, onConfirmRevoke }: McpGrantListProps) {
  const revokeOpeners = useRef(new Map<string, HTMLButtonElement>());
  const cancelButton = useRef<HTMLButtonElement>(null);
  const restoreFocusId = useRef<string | null>(null);

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
          <li key={grant.id} data-mcp-grant={grant.id} aria-busy={confirming && revoking}>
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
                <button ref={cancelButton} type="button" className="secondary-button" disabled={revoking} onClick={cancel}>Cancel</button>
                <button type="button" className="primary-button" aria-describedby={confirmTextId} disabled={revoking} onClick={onConfirmRevoke}>
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
                className="secondary-button"
                aria-label={`Revoke ${grant.clientName} (${shortId})`}
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
