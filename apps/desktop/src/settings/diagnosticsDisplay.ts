import type { DiagnosticsState } from "@peppy/desktop-ui/bridge-contracts";

const BYTES_PER_KIB = 1024;
const BYTES_PER_MIB = 1024 * BYTES_PER_KIB;

/** Only claims retained logs when there are some. */
export function recordingText({ enabled, retainedBytes }: Pick<DiagnosticsState, "enabled" | "retainedBytes">): string {
  if (enabled) return "Active";
  return retainedBytes > 0 ? "Off. Retained logs are still available." : "Off";
}

/** Binary units, matching the "4 MiB" retention cap. */
export function formatDiagnosticsSize(bytes: number): string {
  if (bytes < BYTES_PER_KIB) return `${bytes} B`;
  if (bytes < BYTES_PER_MIB) return `${Math.floor(bytes / BYTES_PER_KIB)} KiB`;
  return `${(bytes / BYTES_PER_MIB).toFixed(1).replace(/\.0$/, "")} MiB`;
}

/** Share of the retention cap in use, clamped to 0 through 100. */
export function diagnosticsUsagePercent(retainedBytes: number, limitBytes: number): number {
  if (limitBytes <= 0) return 0;
  return Math.min(100, Math.max(0, (retainedBytes / limitBytes) * 100));
}
