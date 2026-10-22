import updateStatusFixture from "./update-status.fixture.json";

/** Canonical Rust update-status DTO mirrored for the desktop renderer boundary. */
export type UpdateChannel = "stable" | "staging" | "disabled";
export type UpdatePhase =
  | "disabled"
  | "idle"
  | "checking"
  | "downloading"
  | "ready"
  | "installed"
  | "deferred"
  | "authorization_required"
  | "installing"
  | "error";

export interface UpdateStatus {
  currentVersion: string;
  channel: UpdateChannel;
  phase: UpdatePhase;
  automatic: boolean;
  availableVersion: string | null;
  progressPercent: number | null;
  lastCheckedAt: string | null;
  deferredUntil: string | null;
  message: string | null;
  releaseNotesUrl: string | null;
  canRestart: boolean;
  packageKind: string;
  unattended: boolean;
}

/** Shared Rust/TypeScript serde fixture; Rust includes this JSON in its contract test. */
export const UPDATE_STATUS_SERDE_FIXTURE = updateStatusFixture as UpdateStatus;
