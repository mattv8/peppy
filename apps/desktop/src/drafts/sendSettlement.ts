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

/** Why a close cannot go ahead yet, in order of precedence. */
export type CloseWaitReason = "still-sending" | "reaction-still-sending" | "send-unsent" | "reaction-unsent";

export async function closeWaitOutcome(send: Promise<SendSettlement> | undefined, reactions: Promise<SendSettlement>[], timeoutMs: number): Promise<CloseWaitReason | undefined> {
  const sendResult: { value: SendSettlement | undefined } = { value: send ? undefined : "sent" };
  const reactionResults: (SendSettlement | undefined)[] = reactions.map(() => undefined);
  const waits = reactions.map((reaction, index) => reaction.then((result) => { reactionResults[index] = result; }));
  if (send) waits.push(send.then((result) => { sendResult.value = result; }));
  await waitForSend(Promise.all(waits), timeoutMs);
  if (!sendResult.value) return "still-sending";
  if (reactionResults.some((result) => !result)) return "reaction-still-sending";
  if (sendResult.value === "unsent") return "send-unsent";
  if (reactionResults.includes("unsent")) return "reaction-unsent";
  return undefined;
}
