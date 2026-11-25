export type SendSettlement = "sent" | "unsent";
export const CLOSE_SEND_WAIT_MS = 6_000;

export function createInFlightSend(): { settled: Promise<SendSettlement>; submitted: Promise<boolean>; submit(): void; settle(settlement: SendSettlement): void } {
  let resolveSettled!: (settlement: SendSettlement) => void;
  let resolveSubmitted!: (submitted: boolean) => void;
  const settled = new Promise<SendSettlement>((resolve) => { resolveSettled = resolve; });
  const submitted = new Promise<boolean>((resolve) => { resolveSubmitted = resolve; });
  return { settled, submitted, submit: () => resolveSubmitted(true), settle: (settlement) => { resolveSettled(settlement); resolveSubmitted(false); } };
}

export async function waitForSend<T>(settled: Promise<T>, timeoutMs: number): Promise<T | "still-sending"> {
  let timer: ReturnType<typeof setTimeout> | undefined;
  try {
    return await Promise.race([
      settled,
      new Promise<"still-sending">((resolve) => { timer = setTimeout(() => resolve("still-sending"), timeoutMs); }),
    ]);
  } finally {
    clearTimeout(timer);
  }
}
