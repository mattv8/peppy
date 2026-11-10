import { CopyableSnippet, Steps } from "./CodingAgentSetupPrimitives";

export type OAuthCodingAgentClientId = "claude-desktop" | "chatgpt" | "opencode";

export function CodingAgentOAuthConnection({ clientId, mcpUrl }: { clientId: OAuthCodingAgentClientId; mcpUrl: string }) {
  if (clientId === "claude-desktop") return <Steps>
    <li>Navigate to a connector: personal users choose Customize → Connectors → Add custom connector; organization owners choose Organization settings → Connectors → Add → Custom → Web, if asked.</li>
    <li>Give it a recognizable name and paste the OAuth MCP endpoint:<CopyableSnippet value={mcpUrl} label="Copy OAuth MCP endpoint" /></li>
    <li>Under Authentication choose <strong>Sign in now</strong>.</li>
    <li>Leave <strong>Use Claude&apos;s published identity</strong> selected. <strong>Register automatically</strong> also works if needed.</li>
    <li>Leave Request headers empty. Do not add a client ID, client secret, or vault credential.</li>
    <li>Add the connector, then complete Peppy sign-in and approve the connection if prompted (your existing session may finish without requiring another prompt). Enable the connector in + → Connectors.</li>
  </Steps>;
  if (clientId === "chatgpt") return <Steps>
    <li>Open Settings → Security and login → Developer mode, then enable it if available.</li>
    <li>Open ChatGPT Plugins, select plus, and create a developer app using OAuth.</li>
    <li>Paste the OAuth MCP endpoint: <CopyableSnippet value={mcpUrl} label="Copy OAuth MCP endpoint" /></li>
    <li>Complete Peppy sign-in and approve the connection if prompted, then enable the app in a conversation.</li>
  </Steps>;
  return <Steps>
    <li>Run this command in a local terminal:<CopyableSnippet value={`opencode mcp add peppy --global --url ${JSON.stringify(mcpUrl)}`} label="Copy OpenCode OAuth connection command" /></li>
    <li>Open OpenCode and use <code>/mcps</code> to sign in to Peppy.</li>
    <li>Use <code>/mcps</code> or <code>opencode mcp list</code> to verify the connection status.</li>
  </Steps>;
}
