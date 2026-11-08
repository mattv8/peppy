export function TypingIndicator({ visible, label, scope = "direct" }: { visible: boolean; label: string; scope?: "direct" | "group" }) {
  return <span id="typing-indicator" hidden={!visible} data-typing-scope={scope} data-typing-visible={visible} title={label}>
    <span id="typing-indicator-dots" aria-hidden="true"><i /><i /><i /></span>
    <span id="typing-indicator-label">{label}</span>
  </span>;
}

export function TypingAnnouncer({ text }: { text: string }) {
  return <span id="typing-announcer" className="sr-only" role="status" aria-atomic="true">{text}</span>;
}
