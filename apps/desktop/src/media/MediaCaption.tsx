import type { CSSProperties, ReactNode } from "react";
import "./media-caption.css";

type MediaCaptionProps = {
  messageId: string;
  sender: "self" | "other";
  body?: ReactNode;
  subject?: string;
  toneStyle?: CSSProperties;
};

export function MediaCaption({ messageId, sender, body, subject, toneStyle }: MediaCaptionProps) {
  if (!body && !subject) return null;
  return <div className="media-caption" data-message-caption={messageId}>
    <div className="media-caption-bubble" data-peppy-tone={sender === "other" || undefined} style={toneStyle}>
      {body && <p>{body}</p>}
      {subject && <p className="message-subject">Subject: <bdi>{subject}</bdi></p>}
    </div>
  </div>;
}
