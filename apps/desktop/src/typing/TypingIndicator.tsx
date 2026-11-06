export function TypingIndicator({ visible, label }: { visible: boolean; label: string }) {
  return <span id="typing-indicator" hidden={!visible} data-typing-scope="direct" data-typing-visible={visible}>
    <span id="typing-indicator-dots" aria-hidden="true"><i /><i /><i /></span>
    <span id="typing-indicator-label">{label}</span>
  </span>;
}

export function TypingAnnouncer({ text }: { text: string }) {
  return <span id="typing-announcer" className="sr-only" role="status" aria-atomic="true">{text}</span>;
}
