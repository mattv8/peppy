export function ReactionSettings({ show, onShow }: { show: boolean; onShow(show: boolean): void }) {
  return <section id="settings-messages" className="settings-card" data-settings-section="messages" aria-labelledby="settings-messages-title">
    <h2 id="settings-messages-title">Messages</h2>
    <div className="settings-control-row">
      <label htmlFor="settings-show-reactions">Show reactions</label>
      <span className="settings-switch">
        <input id="settings-show-reactions" type="checkbox" role="switch" checked={show} aria-checked={show} aria-describedby="settings-show-reactions-hint" onChange={(event) => onShow(event.target.checked)} />
        <span aria-hidden />
      </span>
    </div>
    <p id="settings-show-reactions-hint" className="settings-field-hint">Show reaction texts such as “Reacted 😆 to …” as a badge on the original message. Turn off to show them as ordinary messages.</p>
  </section>;
}
