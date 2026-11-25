export type SendSettlement = "sent" | "unsent";
export const CLOSE_SEND_WAIT_MS = 6_000;

export async function waitForSend(settled: Promise<SendSettlement>, timeoutMs: number): Promise<SendSettlement | "still-sending"> {
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
