import type { RefObject } from "react";
import type { McpGrantView } from "@peppy/desktop-ui/bridge-contracts";
import { McpGrantList } from "./McpGrantList";

export type McpGrantsSectionProps = {
  headingRef: RefObject<HTMLHeadingElement | null>;
  manageable: boolean;
  offline: boolean;
  loading: boolean;
  loadError: string;
  grants: McpGrantView[];
  notice: string;
  revokeError: string;
  revokeTargetId: string | null;
  revoking: boolean;
  onRetryLoad(): void;
  onStartRevoke(grant: McpGrantView): void;
  onCancelRevoke(): void;
  onConfirmRevoke(): void;
};

export function McpGrantsSection(props: McpGrantsSectionProps) {
  const { headingRef, manageable, offline, loading, loadError, grants, notice, revokeError, onRetryLoad } = props;
  const body = !manageable ? (
    <p>{offline ? "Authorized clients can't be managed while the server is offline." : "Only an owner can manage authorized clients."}</p>
  ) : loading ? (
    <p role="status">Loading authorized clients…</p>
  ) : loadError ? (
    <>
      <p role="alert">{loadError}</p>
      <button type="button" className="secondary-button" onClick={onRetryLoad}>Retry loading clients</button>
    </>
  ) : grants.length === 0 ? (
    <p>No authorized clients yet. Add the endpoint in your AI client, then approve the request in Peppy.</p>
  ) : (
    <McpGrantList
      grants={grants}
      revokeTargetId={props.revokeTargetId}
      revoking={props.revoking}
      onStartRevoke={props.onStartRevoke}
      onCancelRevoke={props.onCancelRevoke}
      onConfirmRevoke={props.onConfirmRevoke}
    />
  );

  return (
    <section id="mcp-grants" aria-labelledby="mcp-grants-title">
      <h3 id="mcp-grants-title" ref={headingRef} tabIndex={-1}>Authorized clients</h3>
      <p id="mcp-grant-status" role="status" aria-live="polite">{notice}</p>
      {revokeError && <p role="alert">{revokeError}</p>}
      {body}
    </section>
  );
}
