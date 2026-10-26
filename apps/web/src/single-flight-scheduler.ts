export function createSingleFlightTrailingScheduler(run: () => Promise<void>) {
  let disposed = false;
  let inFlight = false;
  let trailing = false;
  const request = () => {
    if (disposed) return;
    if (inFlight) {
      trailing = true;
      return;
    }
    inFlight = true;
    const complete = () => {
      inFlight = false;
      if (!trailing) return;
      trailing = false;
      request();
    };
    try {
      void run().catch(() => undefined).finally(complete);
    } catch {
      complete();
    }
  };
  return { request, dispose: () => { disposed = true; trailing = false; } };
}
