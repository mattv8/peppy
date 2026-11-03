import type { KeyboardEvent } from "react";
import { Image as MediaIcon, MessageSquare } from "@peppy/desktop-ui";

export type ConversationView = "messages" | "media";

type ConversationViewTabsProps = {
  value: ConversationView;
  onChange(next: ConversationView): void;
  variant: "text" | "icons";
};

const tabIdForView = (view: ConversationView) => `tab-${view}`;

export function ConversationViewTabs({ value, onChange, variant }: ConversationViewTabsProps) {
  const handleKeyDown = (event: KeyboardEvent<HTMLDivElement>) => {
    if (event.key !== "ArrowLeft" && event.key !== "ArrowRight") return;
    const next = value === "messages" ? "media" : "messages";
    onChange(next);
    document.getElementById(tabIdForView(next))?.focus();
  };

  const iconTabs = variant === "icons";
  return <div id="conversation-tabs" role="tablist" aria-label="Conversation views" data-variant={variant} onKeyDown={handleKeyDown}>
    <button id="tab-messages" type="button" role="tab" tabIndex={value === "messages" ? 0 : -1} aria-selected={value === "messages"} aria-controls="tabpanel-messages" onClick={() => onChange("messages")} aria-label={iconTabs ? "Messages" : undefined} title={iconTabs ? "Messages" : undefined}>
      {iconTabs ? <MessageSquare size={14} strokeWidth={1.75} aria-hidden /> : "Messages"}
    </button>
    <button id="tab-media" type="button" role="tab" tabIndex={value === "media" ? 0 : -1} aria-selected={value === "media"} aria-controls="tabpanel-media" onClick={() => onChange("media")} aria-label={iconTabs ? "Media" : undefined} title={iconTabs ? "Media" : undefined}>
      {iconTabs ? <MediaIcon size={14} strokeWidth={1.75} aria-hidden /> : "Media"}
    </button>
  </div>;
}
