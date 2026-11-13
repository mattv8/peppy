import { memo } from "react";
import { ContactAvatar, incomingToneSeed, toneStyle, type ToneOverrides } from "@peppy/desktop-ui";

export type TypingRowTyper = { address: string; name?: string; photoDataUrl?: string; toneSeed: string };
export type TypingRowData = { scope: "direct" | "group"; label: string; typers: TypingRowTyper[] };

export const TypingRow = memo(function TypingRow({ scope, label, typers, showSenderAvatars, conversationToneSeed, toneOverrides }: TypingRowData & { showSenderAvatars: boolean; conversationToneSeed: string; toneOverrides: ToneOverrides }) {
  const typer = typers[0];
  const singleGroupTyper = scope === "group" && typers.length === 1 ? typer : undefined;
  const toneSeed = singleGroupTyper?.toneSeed ?? (scope === "direct" ? typer?.toneSeed ?? incomingToneSeed(conversationToneSeed) : undefined);
  const tinted = Boolean(toneSeed);

  return <div id="typing-indicator" className="message-row other" data-typing-row data-typing-scope={scope} data-typing-count={scope === "group" ? typers.length : undefined} data-sender="other" data-sender-gutter={scope === "group" && showSenderAvatars ? "" : undefined} aria-hidden="true">
    {scope === "group" ? <>
      <div className="message-sender-line" data-sender-line>
        {showSenderAvatars && (singleGroupTyper
          ? <span className="message-sender-avatar" data-sender-avatar={singleGroupTyper.address} aria-hidden="true"><ContactAvatar size={24} seed={singleGroupTyper.toneSeed} name={singleGroupTyper.name} photoDataUrl={singleGroupTyper.photoDataUrl} toneOverrides={toneOverrides} /></span>
          : <span className="message-sender-gutter" aria-hidden="true" />)}
        <TypingBubble label={label} tinted={tinted} toneSeed={toneSeed} toneOverrides={toneOverrides} />
      </div>
      <div id="typing-indicator-label" className="message-meta">{label}</div>
    </> : <TypingBubble label={label} tinted={tinted} toneSeed={toneSeed} toneOverrides={toneOverrides} />}
  </div>;
});

function TypingBubble({ label, tinted, toneSeed, toneOverrides }: { label: string; tinted: boolean; toneSeed?: string; toneOverrides: ToneOverrides }) {
  return <article id="typing-indicator-bubble" className="message-bubble other" data-peppy-tone={tinted || undefined} data-typing-bubble style={toneSeed ? toneStyle(toneSeed, toneOverrides) : undefined} title={label}>
    <span id="typing-indicator-dots" className="typing-dots"><i /><i /><i /></span>
  </article>;
}

export function TypingAnnouncer({ text }: { text: string }) {
  return <span id="typing-announcer" className="sr-only" role="status" aria-atomic="true">{text}</span>;
}
