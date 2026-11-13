import { MCP_COMPLETE_REQUEST_TIMEOUT_MS } from "@peppy/generated/mcp";

// Must exceed the server's 15 s long-poll (?wait=15) plus margin for the bounded body read.
export const ENROLLMENT_REQUEST_TIMEOUT_MS = 25_000;
const MCP_ENABLE_NON_COMPLETE_REQUESTS = 7;
const MCP_ENABLE_CRYPTO_MARGIN_MS = 5_000;
/** Covers every enable request at its own deadline, including one replayed completion. */
export const MCP_ENABLE_TIMEOUT_MS = MCP_ENABLE_NON_COMPLETE_REQUESTS * ENROLLMENT_REQUEST_TIMEOUT_MS + 2 * MCP_COMPLETE_REQUEST_TIMEOUT_MS + MCP_ENABLE_CRYPTO_MARGIN_MS;
