import type { ReactNode } from "react";
import { buildClientConfig, buildCursorInstallLink, PEPPY_MCP_SERVER_NAME } from "./codingAgentConfigs";
import { claudeIllustratedExample, claudeSetupImages, codingAgentOfficialDocs } from "./codingAgentGuideResources";
import { getCodingAgentClientLabel, type CodingAgentClientId } from "./codingAgentClients";
import { CodingAgentOAuthConnection } from "./CodingAgentOAuthConnection";
import { ConfigLocations, CopyableSnippet, ExternalLink, GuideImage, GuideNote, Steps } from "./CodingAgentSetupPrimitives";
import "./coding-agent-guide.css";

type GuideProps = { clientId: CodingAgentClientId; mcpUrl: string; onOpenExternal?: (url: string) => void };
type StandardClientId = Exclude<CodingAgentClientId, "claude-desktop" | "chatgpt">;
type GuideSectionProps = Pick<GuideProps, "mcpUrl" | "onOpenExternal">;

const verificationPrompt = "Call list_conversations with limit 5, then summarize the vault's recent conversations and the tools you can use. Do not send messages or reactions.";
const beforeStartCopy: Record<StandardClientId, ReactNode> = {
  "claude-code": "Claude Code signs in with OAuth. No vault credential is needed.",
  cursor: "Cursor signs in with OAuth. No vault credential is needed.",
  cline: <>Cline signs in with OAuth. No vault credential is needed. When Cline shows <strong>Authenticate</strong>, select it to sign in to Peppy.</>,
  continue: <>Continue signs in with OAuth when Peppy asks. No vault credential or <code>.env</code> entry is needed. Use Agent mode.</>,
  opencode: "OpenCode signs in with OAuth. No vault credential is needed.",
  hermes: <>Hermes signs in with OAuth (<code>auth: oauth</code>). No vault credential is needed.</>,
  codex: "Codex signs in with OAuth. No vault credential is needed.",
};

function Alternative({ summary, disclosure, children }: { summary: string; disclosure: string; children: ReactNode }) {
  return <details className="coding-agent-guide-alternative" data-guide-disclosure={disclosure}><summary>{summary}</summary>{children}</details>;
}

function Docs({ href, onOpenExternal }: { href: string; onOpenExternal?: (url: string) => void }) {
  return <section className="coding-agent-guide-section" data-guide-section="official-docs"><h3>Official docs</h3><ExternalLink href={href} onOpenExternal={onOpenExternal}>Open the current client documentation</ExternalLink></section>;
}

function Verify() {
  return <section className="coding-agent-guide-section" data-guide-section="verify"><h3>Verify</h3><CopyableSnippet value={verificationPrompt} label="Copy safe verification prompt" /></section>;
}

function ClaudeGuide({ mcpUrl, onOpenExternal }: GuideSectionProps) {
  return <>
    <section className="coding-agent-guide-section" data-guide-section="claude-remote"><h3>Before you start</h3><p>The endpoint must be public HTTPS reachable from Anthropic&apos;s cloud. Your own VPN access is not enough.</p></section>
    <section className="coding-agent-guide-section" data-guide-section="claude-connect"><h3>Connect Claude Cowork and Desktop</h3><CodingAgentOAuthConnection clientId="claude-desktop" mcpUrl={mcpUrl} /><GuideNote>OAuth uses your signed-in Peppy account and the permissions you approve. Revoke it in Claude&apos;s Connectors settings or in Peppy MCP Details.</GuideNote></section>
    <Verify />
    <Alternative summary="Troubleshooting and changes" disclosure="claude-troubleshooting"><p>If sign-in or tools fail, check that Peppy MCP shows Online in Settings → Devices, then remove and re-add the connector.</p></Alternative>
    <section className="coding-agent-guide-section" data-guide-section="claude-illustrated-example">
      <h3>Illustrated example</h3>
      <ExternalLink href={claudeIllustratedExample.href} onOpenExternal={onOpenExternal}>{claudeIllustratedExample.label}</ExternalLink>
      <small>{claudeIllustratedExample.source}; follow the current dialog rather than older screenshots.</small>
      <Alternative summary="View older remote connector example" disclosure="claude-remote-image"><GuideImage {...claudeSetupImages.remote} onOpenExternal={onOpenExternal} /></Alternative>
    </section>
    <Docs href={codingAgentOfficialDocs.claude} onOpenExternal={onOpenExternal} />
  </>;
}

function ChatGptGuide({ mcpUrl, onOpenExternal }: GuideSectionProps) {
  return <>
    <section className="coding-agent-guide-section" data-guide-section="chatgpt-before"><h3>Before you start</h3><p>Hosted ChatGPT requires OAuth to access Peppy tools. Developer mode is available on your plan and organization if applicable. No vault credential is needed.</p></section>
    <section className="coding-agent-guide-section" data-guide-section="connect"><h3>Connect</h3><CodingAgentOAuthConnection clientId="chatgpt" mcpUrl={mcpUrl} /><p>The default <code>/mcp</code> route provides Peppy tools; custom routes do not.</p></section>
    <Verify />
    <Alternative summary="Troubleshooting" disclosure="chatgpt-troubleshooting"><p>If OAuth is unavailable, check that Peppy MCP is enabled and Online in Settings → Devices.</p></Alternative>
    <Docs href={codingAgentOfficialDocs.chatgpt} onOpenExternal={onOpenExternal} />
  </>;
}

function standardLocations(clientId: StandardClientId) {
  const locations: Record<StandardClientId, { label: string; path: string }[]> = {
    "claude-code": [{ label: "Project", path: ".mcp.json" }, { label: "User", path: "~/.claude.json" }],
    cursor: [{ label: "Project", path: ".cursor/mcp.json" }, { label: "User", path: "~/.cursor/mcp.json" }],
    cline: [{ label: "UI opener", path: "MCP Servers → Configure → Configure MCP Servers" }],
    continue: [{ label: "Standalone", path: ".continue/mcpServers/peppy.yaml" }, { label: "User merge", path: "~/.continue/config.yaml" }],
    opencode: [{ label: "Global", path: "~/.config/opencode/opencode.json" }, { label: "Project", path: "opencode.json" }],
    hermes: [{ label: "Configuration", path: "~/.hermes/config.yaml" }, { label: "Tokens", path: "~/.hermes/mcp-tokens/" }],
    codex: [{ label: "User", path: "~/.codex/config.toml" }, { label: "Trusted project", path: ".codex/config.toml" }],
  };
  return locations[clientId];
}

function completion(clientId: StandardClientId): ReactNode {
  const text: Record<StandardClientId, ReactNode> = {
    "claude-code": <>Run <code>claude mcp get {PEPPY_MCP_SERVER_NAME}</code>, then confirm the named tools appear.</>,
    cursor: "Open Cursor Customize and MCP Logs, then confirm the named MCP server connects.",
    cline: "Save through the Configure MCP Servers opener, restart Cline or open a new task, and confirm the named tools appear.",
    continue: "Save the configuration, let Continue refresh, switch to Agent mode, and confirm the named MCP tools appear.",
    opencode: <>Run <code>/mcps</code> or <code>opencode mcp list</code>, then confirm the named server appears.</>,
    hermes: <>Run <code>/reload-mcp</code> then <code>hermes mcp test {PEPPY_MCP_SERVER_NAME}</code>.</>,
    codex: <>Restart Codex, run <code>/mcp</code>, and confirm the named server appears.</>,
  };
  return text[clientId];
}

function troubleshooting(clientId: StandardClientId): ReactNode {
  const text: Record<StandardClientId, ReactNode> = {
    "claude-code": <>Use <code>claude mcp get {PEPPY_MCP_SERVER_NAME}</code> to inspect the configured server.</>,
    cursor: "Open MCP Logs after install to diagnose connection errors.",
    cline: <>Use <code>streamableHttp</code>, not Cline&apos;s legacy SSE default; the UI Configure MCP Servers opener is authoritative.</>,
    continue: <>Use Agent mode. The standalone YAML has metadata; when using the user config, merge only <code>mcpServers</code> and preserve other settings.</>,
    opencode: <>Use V2 <code>mcp.servers</code>. Run <code>/mcps</code> or <code>opencode mcp list</code>.</>,
    hermes: <>If sign-in times out inside a session, run <code>hermes mcp login {PEPPY_MCP_SERVER_NAME}</code> from a fresh terminal.</>,
    codex: <>Trust the project config if used, run <code>codex mcp login {PEPPY_MCP_SERVER_NAME}</code> again, then run <code>/mcp</code>.</>,
  };
  return text[clientId];
}

function ClaudeCodeConnection({ mcpUrl, config }: { mcpUrl: string; config: string }) {
  return <><Steps><li>Use the current CLI for user scope, or use the project <code>.mcp.json</code> alternative.</li></Steps><CopyableSnippet value={`claude mcp add --scope user --transport http peppy ${JSON.stringify(mcpUrl)}`} label="Copy Claude Code user-scope command" /><Steps start={2}><li>Open Claude Code, run <code>/mcp</code>, select <strong>peppy</strong>, and sign in.</li></Steps><Alternative summary="Project configuration alternative" disclosure="claude-code-project-config"><CopyableSnippet value={config} label="Copy Claude Code project configuration" /></Alternative></>;
}

function CursorConnection({ mcpUrl, config, onOpenExternal }: { mcpUrl: string; config: string; onOpenExternal?: (url: string) => void }) {
  return <><ExternalLink className="primary-button" href={buildCursorInstallLink(mcpUrl)} onOpenExternal={onOpenExternal}>Install in Cursor</ExternalLink><p>Review the install entry. Then select <strong>Connect</strong> next to peppy in Cursor&apos;s MCP settings to sign in.</p><Alternative summary="Manual JSON alternative" disclosure="cursor-manual-config"><CopyableSnippet value={config} label="Copy Cursor configuration" /></Alternative></>;
}

function StandardConnection({ clientId, clientLabel, mcpUrl, config, onOpenExternal }: { clientId: StandardClientId; clientLabel: string; mcpUrl: string; config: string; onOpenExternal?: (url: string) => void }) {
  switch (clientId) {
    case "claude-code": return <ClaudeCodeConnection mcpUrl={mcpUrl} config={config} />;
    case "cursor": return <CursorConnection mcpUrl={mcpUrl} config={config} onOpenExternal={onOpenExternal} />;
    case "opencode": return <><CodingAgentOAuthConnection clientId="opencode" mcpUrl={mcpUrl} /><Alternative summary="Configuration alternative" disclosure="opencode-config"><CopyableSnippet value={config} label="Copy OpenCode configuration" /></Alternative></>;
    case "hermes": return <><CopyableSnippet value={`hermes mcp add --url ${JSON.stringify(mcpUrl)} --auth oauth peppy`} label="Copy Hermes command" /><Alternative summary="Configuration alternative" disclosure="hermes-config"><CopyableSnippet value={config} label="Copy Hermes configuration" /></Alternative></>;
    case "codex": return <><CopyableSnippet value={config} label="Copy Codex configuration" /><p>Then sign in:</p><CopyableSnippet value="codex mcp login peppy" label="Copy Codex sign-in command" /></>;
    case "cline":
    case "continue": return <CopyableSnippet value={config} label={`Copy ${clientLabel} configuration`} />;
  }
}

function StandardGuide({ clientId, clientLabel, mcpUrl, onOpenExternal }: { clientId: StandardClientId; clientLabel: string; mcpUrl: string; onOpenExternal?: (url: string) => void }) {
  const config = buildClientConfig(clientId, mcpUrl)!;
  const docs = clientId === "claude-code" ? codingAgentOfficialDocs.claudeCode : codingAgentOfficialDocs[clientId];
  return <>
    <section className="coding-agent-guide-section" data-guide-section="before-start"><h3>Before you start</h3><p>{beforeStartCopy[clientId]}</p></section>
    <section className="coding-agent-guide-section" data-guide-section="connect"><h3>Connect</h3><StandardConnection clientId={clientId} clientLabel={clientLabel} mcpUrl={mcpUrl} config={config} onOpenExternal={onOpenExternal} /><ConfigLocations locations={standardLocations(clientId)} /></section>
    <p className="coding-agent-guide-completion">{completion(clientId)}</p>
    <Verify />
    <Alternative summary="Troubleshooting" disclosure={`${clientId}-troubleshooting`}><p>{troubleshooting(clientId)}</p></Alternative>
    <Docs href={docs} onOpenExternal={onOpenExternal} />
  </>;
}

export function CodingAgentClientGuide({ clientId, mcpUrl, onOpenExternal }: GuideProps) {
  const clientLabel = getCodingAgentClientLabel(clientId);
  return <section className="coding-agent-client-guide" data-client-guide={clientId} aria-label={`${clientLabel} setup guide`}>
    <p className="coding-agent-guide-description">Connect {clientLabel} to this vault MCP endpoint.</p>
    {clientId === "claude-desktop" ? <ClaudeGuide mcpUrl={mcpUrl} onOpenExternal={onOpenExternal} /> : clientId === "chatgpt" ? <ChatGptGuide mcpUrl={mcpUrl} onOpenExternal={onOpenExternal} /> : <StandardGuide clientId={clientId} clientLabel={clientLabel} mcpUrl={mcpUrl} onOpenExternal={onOpenExternal} />}
  </section>;
}
