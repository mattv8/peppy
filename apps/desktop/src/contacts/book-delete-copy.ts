/** Why deleting a removed phone's book for every device is unavailable right now, in priority order. */
export const DELETE_LOCKED_REASON = "Unlock sync to delete this book for all devices.";
export const DELETE_OFFLINE_REASON = "Connect to the server to delete this book for all devices.";
export const DELETE_SYNCING_REASON = "Finish syncing to delete this book for all devices.";

/** The bridge's refusal codes that the view reports with the same copy as the disabled reasons. */
export function deleteRefusalText(code: string | undefined): string | undefined {
  switch (code) {
    case "locked": return DELETE_LOCKED_REASON;
    case "offline": return DELETE_OFFLINE_REASON;
    case "sync-not-current": return DELETE_SYNCING_REASON;
    default: return undefined;
  }
}
