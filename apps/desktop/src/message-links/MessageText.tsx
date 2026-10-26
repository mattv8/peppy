import { useEffect, useMemo, useRef, useState, type MouseEvent } from "react";
import { tokenizeMessageText } from "./tokenize";
import "./message-links.css";

export type MessageTextProps = {
  text: string;
  onOpenExternal?: (url: string) => Promise<void>;
};

type OpenFailure = {
  href: string;
  textRevision: number;
};

type CopyState = "copied" | "failed";

export function MessageText({ text, onOpenExternal }: MessageTextProps) {
  const [openFailure, setOpenFailure] = useState<OpenFailure>();
  const [copyState, setCopyState] = useState<CopyState>();
  const previousText = useRef(text);
  const textRevision = useRef(0);
  const tokens = useMemo(() => tokenizeMessageText(text), [text]);

  if (previousText.current !== text) {
    previousText.current = text;
    textRevision.current += 1;
  }

  useEffect(() => {
    setOpenFailure(undefined);
    setCopyState(undefined);
  }, [text]);

  const currentOpenFailure = openFailure?.textRevision === textRevision.current ? openFailure : undefined;

  const openExternally = (href: string) => {
    if (!onOpenExternal) return;

    const openingTextRevision = textRevision.current;
    setCopyState(undefined);
    void onOpenExternal(href).then(
      () => setOpenFailure(current => current?.href === href && current.textRevision === openingTextRevision ? undefined : current),
      () => {
        if (textRevision.current === openingTextRevision) setOpenFailure({ href, textRevision: openingTextRevision });
      },
    );
  };

  const handleClick = (event: MouseEvent<HTMLAnchorElement>, href: string) => {
    if (!onOpenExternal) return;

    event.preventDefault();
    if (event.button === 0) openExternally(href);
  };

  const handleAuxClick = (event: MouseEvent<HTMLAnchorElement>, href: string) => {
    if (!onOpenExternal || event.button !== 1) return;

    event.preventDefault();
    openExternally(href);
  };

  const copyLink = (href: string) => {
    const clipboard = navigator.clipboard;
    if (!clipboard) {
      setCopyState("failed");
      return;
    }

    void clipboard.writeText(href).then(
      () => setCopyState("copied"),
      () => setCopyState("failed"),
    );
  };

  return (
    <span data-message-text>
      {tokens.map((token, index) => token.kind === "text"
        ? <span key={index}>{token.text}</span>
        : <a key={index} data-message-text-link className="message-text-link" data-message-text-destination={token.href} href={token.href} title={token.href} target="_blank" rel="noopener noreferrer" onClick={event => handleClick(event, token.href)} onAuxClick={event => handleAuxClick(event, token.href)}>{token.text}</a>)}
      {currentOpenFailure && (
        <span data-message-text-open-alert className="message-text-open-failure" role="alert">
          Could not open link. <button type="button" className="secondary-button" aria-label="Retry opening link" onClick={() => openExternally(currentOpenFailure.href)}>Retry</button> <button type="button" className="secondary-button" aria-label="Copy link" onClick={() => copyLink(currentOpenFailure.href)}>Copy</button>
          {copyState === "copied" && " Link copied."}
          {copyState === "failed" && " Could not copy link."}
        </span>
      )}
    </span>
  );
}
