import { toDate } from "@peppy/desktop-ui";

const BURST_WINDOW_MS = 60 * 60 * 1000;

type TimestampedMessage = {
  id: string;
  timestamp: string;
};

export function hiddenTimeIds<T extends TimestampedMessage>(
  messages: readonly T[],
  shown: (message: T) => boolean,
): Set<string> {
  const hiddenIds = new Set<string>();
  let previousTime: number | undefined;
  let keeperId: string | undefined;

  for (const message of messages) {
    const time = toDate(message.timestamp)?.getTime();
    const beginsBurst = time === undefined
      || previousTime === undefined
      || Math.abs(time - previousTime) > BURST_WINDOW_MS;

    if (beginsBurst) keeperId = undefined;
    if (shown(message)) {
      if (keeperId !== undefined) hiddenIds.add(keeperId);
      keeperId = message.id;
    }
    previousTime = time;
  }

  return hiddenIds;
}
