export function TypingIndicator({ visible }: { visible: boolean }) {
  return <span id="typing-indicator" hidden={!visible} data-typing-scope="direct" data-typing-visible={visible} title="Typing">
    <span id="typing-indicator-dots" aria-hidden="true"><i /><i /><i /></span>
    <span id="typing-indicator-label">Typing</span>
  </span>;
}

export function TypingAnnouncer({ text }: { text: string }) {
  return <span id="typing-announcer" className="sr-only" role="status" aria-atomic="true">{text}</span>;
}
