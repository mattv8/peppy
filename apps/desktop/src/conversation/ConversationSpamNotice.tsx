import type { JSX } from "react";

type Props = { folder?: "inbox" | "archived" | "spam"; folderReason?: "marked" | "blocked"; unblock?: "pending" | "failed" };

export function ConversationSpamNotice({ folder, folderReason, unblock }: Props): JSX.Element {
  const text = unblock === "pending" ? "Unblocking on your phone…"
    : unblock === "failed" ? "Your phone couldn't unblock this number. Unblock it in your messaging app."
      : folder === "spam" && folderReason === "blocked" ? "Blocked in your messaging app. New texts from this number don't arrive. Use Not spam to unblock it."
        : folder === "spam" && folderReason === "marked" ? "You marked this as spam. It sends no notifications."
          : "";
  return <div id="conversation-spam-notice" role="status">{text}</div>;
}
