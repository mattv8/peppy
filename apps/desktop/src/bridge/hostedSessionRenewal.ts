const ATTEMPT_INTERVAL_MS = 30_000;

export function createHostedSessionRenewal(renew: () => Promise<void>) {
  let renewalInFlight: Promise<void> | undefined;
  let lastAttemptAt = 0;
  const handleInteraction = (event: Pick<Event, "isTrusted">) => {
    if (!event.isTrusted || document.visibilityState !== "visible" || renewalInFlight || Date.now() - lastAttemptAt < ATTEMPT_INTERVAL_MS) return;
    lastAttemptAt = Date.now();
    renewalInFlight = renew().catch(() => {}).finally(() => {
      renewalInFlight = undefined;
    });
  };
  return { handleInteraction };
}

export function installHostedSessionRenewal(renew: () => Promise<void>): () => void {
  if (new URLSearchParams(window.location.search).get("window") === "composer") return () => {};
  const scheduler = createHostedSessionRenewal(renew);
  const events = ["pointerdown", "keydown", "touchstart", "wheel"];
  const options = { capture: true, passive: true };
  events.forEach((event) => document.addEventListener(event, scheduler.handleInteraction, options));
  return () => events.forEach((event) => document.removeEventListener(event, scheduler.handleInteraction, true));
}
