import type { MessageView } from "@peppy/desktop-ui/bridge-contracts";

export type EarlierRisk = "none" | "possible" | "sent";
export type ResendChain = { attempts: MessageView[]; earlierRisk: EarlierRisk };
export type ResendChains = { hidden: ReadonlySet<string>; byLatest: ReadonlyMap<string, ResendChain> };

export function registerResendRows(chain: ResendChain | undefined, row: HTMLElement | null, register: (id: string, row: HTMLElement | null) => void): void {
  chain?.attempts.slice(0, -1).forEach((attempt) => register(attempt.id, row));
}

export function resendChains(messages: readonly MessageView[], options: {
  transientLink?: (messageId: string) => string | undefined;
  keepVisible?: (message: MessageView) => boolean;
}): ResendChains {
  const hidden = new Set<string>();
  const byLatest = new Map<string, ResendChain>();
  const link = (message: MessageView) =>
    message.sentAgain?.messageId ?? message.sendRecovery?.sentAgain?.messageId ?? options.transientLink?.(message.id);

  for (let index = 0; index < messages.length; index += 1) {
    const attempts = [messages[index]];
    const seen = new Set([messages[index].id]);
    let cursor = index;
    while (link(messages[cursor]) !== undefined) {
      const next = messages[cursor + 1];
      if (!next || messages[cursor].sender !== "self" || next.sender !== "self" ||
          link(messages[cursor]) !== next.id || seen.has(next.id)) break;
      attempts.push(next);
      seen.add(next.id);
      cursor += 1;
    }
    // Keep the chain expanded while its latest link is pending or unmatched.
    if (attempts.length < 2 || link(messages[cursor]) !== undefined) continue;
    index = cursor;
    const earlier = attempts.slice(0, -1);
    if (earlier.some((message) => options.keepVisible?.(message))) continue;
    const earlierRisk: EarlierRisk = earlier.some((message) =>
      message.status === "sent" || message.status === "delivery-confirmed" || message.status === "read")
      ? "sent"
      : earlier.every((message) => message.sendRecovery?.kind === "not-sent" ||
        message.status === "failed-before-submit" || message.status === "failed-confirmed")
        ? "none" : "possible";
    earlier.forEach((message) => hidden.add(message.id));
    byLatest.set(attempts[attempts.length - 1].id, { attempts, earlierRisk });
  }
  return { hidden, byLatest };
}
