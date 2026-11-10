import { useEffect, useRef, useState } from "react";
import { AgentBrandIcon, Copy, Info, Modal } from "@peppy/desktop-ui";
import { bridge } from "../bridge/bridge";
import { isRemovedDevice } from "./mcpDisplay";
import { useMcpStatus } from "./mcpStatusStore";
import { CODING_AGENT_CLIENTS, type CodingAgentClientId } from "./agents/codingAgentClients";
import { CodingAgentClientGuide } from "./agents/CodingAgentClientGuide";
import "./mcp-connect.css";

type McpConnectHelpProps = {
  sessionKey: string;
  browserHost: boolean;
};

const COPIED_DURATION_MS = 2_000;
const COPY_FAILED_MESSAGE = "Couldn't copy. Select the endpoint above.";

export function McpConnectHelp({ sessionKey, browserHost }: McpConnectHelpProps) {
  const status = useMcpStatus(sessionKey);
  const [setupOpen, setSetupOpen] = useState(false);
  const [selected, setSelected] = useState<CodingAgentClientId>("claude-desktop");
  const [copyStatus, setCopyStatus] = useState<"copied" | "failed" | null>(null);
  const [linkFailedUrl, setLinkFailedUrl] = useState<string | null>(null);
  const opener = useRef<HTMLElement>(null);
  const copyTimer = useRef<number | null>(null);
  const guideScroll = useRef<HTMLDivElement>(null);
  const tabs = useRef<HTMLDivElement>(null);
  const mounted = useRef(false);

  useEffect(() => {
    mounted.current = true;
    return () => {
      mounted.current = false;
      if (copyTimer.current !== null) window.clearTimeout(copyTimer.current);
    };
  }, []);

  useEffect(() => {
    if (!setupOpen) return;
    guideScroll.current?.scrollTo?.({ top: 0 });
    tabs.current?.querySelector<HTMLButtonElement>('[aria-pressed="true"]')?.scrollIntoView?.({ inline: "nearest", block: "nearest" });
  }, [selected, setupOpen]);

  if (!status?.device || isRemovedDevice(status.device) || status.device.cleanupPending) return null;

  const endpoint = `${status.serverOrigin}/mcp`;
  const openSetup = (clientId: CodingAgentClientId, openerElement: HTMLElement) => {
    opener.current = openerElement;
    setSelected(clientId);
    setSetupOpen(true);
  };
  const copyEndpoint = async () => {
    if (copyTimer.current !== null) window.clearTimeout(copyTimer.current);
    try {
      if (!navigator.clipboard?.writeText) throw new Error("clipboard unavailable");
      await navigator.clipboard.writeText(endpoint);
      if (!mounted.current) return;
      setCopyStatus("copied");
      copyTimer.current = window.setTimeout(() => setCopyStatus(null), COPIED_DURATION_MS);
    } catch {
      if (mounted.current) {
        setCopyStatus("failed");
        copyTimer.current = window.setTimeout(() => setCopyStatus(null), 5_000);
      }
    }
  };
  const openExternal = (url: string) => {
    setLinkFailedUrl(null);
    const showLinkFailure = () => {
      if (!mounted.current) return;
      setLinkFailedUrl(url);
      guideScroll.current?.scrollTo?.({ top: 0 });
    };
    if (!bridge.open_external_url) {
      showLinkFailure();
      return;
    }
    void bridge.open_external_url(url).catch(() => {
      showLinkFailure();
    });
  };

  return <div id="mcp-connect" data-mcp-connect>
    <span id="mcp-connect-label">Peppy MCP endpoint</span>
    <div id="mcp-connect-endpoint" role="group" aria-labelledby="mcp-connect-label">
      <code tabIndex={0} aria-labelledby="mcp-connect-label">{endpoint}</code>
      <button id="mcp-connect-help" type="button" className="info-button" aria-label="How to connect a coding agent" onClick={event => openSetup("claude-desktop", event.currentTarget)}><Info size={16} aria-hidden /></button>
      <span data-mcp-connect-spacer />
      <button id="mcp-connect-copy" type="button" className="secondary-button" onClick={() => void copyEndpoint()}><Copy size={14} aria-hidden />{copyStatus === "copied" ? "Copied" : "Copy"}</button>
    </div>
    <span id="mcp-connect-copy-status" role="status" aria-live="polite" aria-atomic="true" data-copy-state={copyStatus ?? "idle"}>{copyStatus === "copied" ? "Endpoint copied." : copyStatus === "failed" ? COPY_FAILED_MESSAGE : ""}</span>
    <div id="mcp-connect-agents" data-mcp-connect-agents>
      <button type="button" data-mcp-connect-agent-trigger onClick={event => openSetup("claude-desktop", event.currentTarget)}>Connect your agent</button>
      <div id="mcp-connect-agent-icons" role="group" aria-label="Set up a specific coding agent">
        {CODING_AGENT_CLIENTS.map(client => <button key={client.id} type="button" data-agent-client={client.id} aria-label={`Set up ${client.label}`} title={`Set up ${client.label}`} onClick={event => openSetup(client.id, event.currentTarget)}><AgentBrandIcon clientId={client.id} size={16} /></button>)}
      </div>
    </div>
    {setupOpen && <Modal
      id="mcp-agent-setup"
      title="Choose your coding agent"
      className="mcp-agent-setup-modal"
      opener={opener}
      onDismiss={() => setSetupOpen(false)}
      actions={<button type="button" className="secondary-button" onClick={() => setSetupOpen(false)}>Close</button>}
    >
      <div id="mcp-agent-setup-body">
        <p>Coding agents can connect to Peppy MCP and access your vault. They sign in with OAuth; no credential is needed.</p>
        <div ref={tabs} role="group" aria-label="Coding agent" id="mcp-agent-setup-tabs">
          {CODING_AGENT_CLIENTS.map(client => <button key={client.id} type="button" data-agent-client={client.id} aria-pressed={selected === client.id} onClick={() => setSelected(client.id)}><AgentBrandIcon clientId={client.id} size={14} />{client.label}</button>)}
        </div>
        <span id="mcp-agent-setup-guide-status" role="status" aria-live="polite">Showing {CODING_AGENT_CLIENTS.find(client => client.id === selected)?.label} guide</span>
        <div id="mcp-agent-setup-guide" ref={guideScroll}>
          {linkFailedUrl && <p id="mcp-agent-setup-link-error" role="alert">Couldn't open the link. <code tabIndex={0}>{linkFailedUrl}</code></p>}
          <CodingAgentClientGuide clientId={selected} mcpUrl={endpoint} onOpenExternal={browserHost ? undefined : openExternal} />
        </div>
      </div>
    </Modal>}
  </div>;
}
