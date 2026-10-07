export interface OwnerLockManager {
  request<T>(name: string, options: {mode:"exclusive";ifAvailable:true}, operation:(lock:{name:string}|null)=>Promise<T>): Promise<T>;
}

export class OwnerError extends Error {
  public constructor(public readonly code:"already-open"|"cancelled"|"unsupported"|"unavailable") {
    super("Browser worker ownership is unavailable.");
  }
}

export interface WorkerOwner<T> {
  readonly owner: T;
  release(): Promise<void>;
}

export interface OwnerOptions<T> {
  locks: OwnerLockManager | undefined;
  name: string;
  signal?: AbortSignal;
  start(signal: AbortSignal): Promise<T>;
  stop(owner: T): Promise<void>;
}

function deferred<T>() {
  let resolve!: (value: T) => void;
  let reject!: (error: unknown) => void;
  const promise = new Promise<T>((done, failed) => { resolve = done; reject = failed; });
  return { promise, resolve, reject };
}

/** Called inside a SharedWorker. Never steals a lock from an older or suspended worker. */
export async function acquireWorkerOwner<T>(options: OwnerOptions<T>): Promise<WorkerOwner<T>> {
  if (!options.locks) throw new OwnerError("unsupported");
  if (options.signal?.aborted) throw new OwnerError("cancelled");
  if (!options.name || options.name.length > 256) throw new OwnerError("unavailable");
  const locks = options.locks;
  const ready = deferred<WorkerOwner<T>>();
  const released = deferred<void>();
  const finished = deferred<void>();
  const lifetime = new AbortController();
  let stopRequested = false;
  // Boot failures can occur before a caller receives a lease and can await release().
  void finished.promise.catch(() => undefined);

  const release = () => {
    if (!stopRequested) {
      stopRequested = true;
      lifetime.abort();
      released.resolve();
    }
    return finished.promise;
  };
  const cancel = () => { void release().catch(() => undefined); };
  options.signal?.addEventListener("abort", cancel, { once: true });

  const holdLock = async () => {
    try {
      await locks.request(options.name, {mode:"exclusive",ifAvailable:true}, async lock => {
        if (!lock) throw new OwnerError("already-open");
        if (stopRequested) throw new OwnerError("cancelled");
        const owner = await options.start(lifetime.signal);
        if (!stopRequested) ready.resolve({owner,release});
        await released.promise;
        try {
          await options.stop(owner);
        } catch {
          const error = new OwnerError("unavailable");
          ready.reject(error);
          finished.reject(error);
          options.signal?.removeEventListener("abort", cancel);
          // If teardown is uncertain, keep ownership until the worker itself is terminated.
          await new Promise<void>(() => undefined);
        }
      });
      ready.reject(new OwnerError("cancelled"));
      finished.resolve();
    } catch (error: unknown) {
      lifetime.abort();
      const safeError = error instanceof OwnerError ? error : new OwnerError("unavailable");
      ready.reject(safeError);
      finished.reject(safeError);
    } finally {
      options.signal?.removeEventListener("abort", cancel);
    }
  };
  void holdLock();
  return ready.promise;
}
