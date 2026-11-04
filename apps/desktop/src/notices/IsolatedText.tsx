/** Paragraph breakers or PDI can reset inline isolation early. */
const ISOLATE_BREAKER = /[\u2069\u000A\u000D\u001C-\u001E\u0085\u2029]/;

export function IsolatedText({ text }: { text: string }) {
  return <bdi style={ISOLATE_BREAKER.test(text) ? { display: "inline-block" } : undefined}>{text}</bdi>;
}
