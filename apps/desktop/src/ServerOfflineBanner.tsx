import { TriangleAlert } from "@peppy/desktop-ui";
import { peppyCopy } from "./generated/peppyCopy";

export function ServerOfflineBanner() {
  return (
    <section id="server-offline-banner" className="server-offline-banner">
      <TriangleAlert size={16} aria-hidden />
      <div>
        <strong>{peppyCopy.server_offline_title}</strong>
        <span>{peppyCopy.server_offline_body}</span>
      </div>
    </section>
  );
}

export function ServerOfflineAnnouncement({ offline }: { offline: boolean }) {
  return <div id="server-offline-announcement" className="visually-hidden" role="status" aria-live="polite" aria-atomic="true">
    {offline ? `${peppyCopy.server_offline_title}. ${peppyCopy.server_offline_body}` : ""}
  </div>;
}
