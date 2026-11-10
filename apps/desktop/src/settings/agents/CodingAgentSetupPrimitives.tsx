import { Copy } from "@peppy/desktop-ui";
import { useEffect, useRef, useState, type ReactNode } from "react";

const COPIED_DURATION_MS = 1_500;

export function Steps({ children, start }: { children: ReactNode; start?: number }) {
  return <ol className="coding-agent-guide-steps" start={start}>{children}</ol>;
}

export function ExternalLink({ href, children, className, onOpenExternal }: { href: string; children: ReactNode; className?: string; onOpenExternal?: (url: string) => void }) {
  return onOpenExternal
    ? <a className={className} href={href} onClick={(event) => { event.preventDefault(); onOpenExternal(href); }}>{children}</a>
    : <a className={className} href={href} target="_blank" rel="noopener noreferrer">{children}</a>;
}

export function CopyableSnippet({ value, label = "Copy configuration" }: { value: string; label?: string }) {
  const [state, setState] = useState<"idle" | "copied" | "failed">("idle");
  const resetTimer = useRef<number | null>(null);
  const mounted = useRef(true);
  useEffect(() => {
    mounted.current = true;
    return () => {
      mounted.current = false;
      if (resetTimer.current !== null) window.clearTimeout(resetTimer.current);
    };
  }, []);

  const caption = label.replace(/^Copy\s+/, "");
  const copy = async () => {
    if (resetTimer.current !== null) window.clearTimeout(resetTimer.current);
    setState("idle");
    try {
      await navigator.clipboard.writeText(value);
      if (!mounted.current) return;
      setState("copied");
      resetTimer.current = window.setTimeout(() => {
        if (mounted.current) setState("idle");
      }, COPIED_DURATION_MS);
    } catch {
      if (mounted.current) setState("failed");
    }
  };
  const buttonText = state === "copied" ? "Copied" : "Copy";

  return <div className="coding-agent-copyable-snippet" data-setup-snippet={label}>
    <div className="coding-agent-copyable-snippet-toolbar">
      <span className="coding-agent-copyable-snippet-label">{caption}</span>
      <button type="button" className="secondary-button" onClick={() => void copy()} aria-label={`${buttonText} ${caption}`}>
        <Copy size={14} aria-hidden="true" /> {buttonText}
      </button>
    </div>
    <p className="coding-agent-copy-status" role="status" aria-live="polite" aria-atomic="true">{state === "copied" ? `${caption} copied.` : state === "failed" ? "Couldn't copy. Copy it manually." : ""}</p>
    <pre tabIndex={0} aria-label={caption}><code>{value}</code></pre>
  </div>;
}

type ConfigLocation = string | { label: string; path: string };

export function ConfigLocations({ locations }: { locations: ConfigLocation[] }) {
  return <ul className="coding-agent-config-locations" data-setup-config-locations>
    {locations.map((location) => <li className="coding-agent-config-location" key={typeof location === "string" ? location : `${location.label}-${location.path}`}>
      {typeof location === "string" ? <code>{location}</code> : <><span className="coding-agent-config-location-label">{location.label}</span><code>{location.path}</code></>}
    </li>)}
  </ul>;
}

export function GuideNote({ children }: { children: ReactNode }) {
  return <p className="coding-agent-guide-note" role="note">{children}</p>;
}

export function GuideImage({ src, alt, caption, href, onOpenExternal }: { src: string; alt: string; caption: string; href: string; onOpenExternal?: (url: string) => void }) {
  const [failed, setFailed] = useState(false);
  return <figure className="coding-agent-guide-image" data-guide-image={src}>
    {failed ? <p>The example image could not load. <ExternalLink href={href} onOpenExternal={onOpenExternal}>Open the illustrated source instead.</ExternalLink></p> : <img src={src} alt={alt} loading="lazy" onError={() => setFailed(true)} />}
    <figcaption>{caption} <ExternalLink href={href} onOpenExternal={onOpenExternal}>Source and illustrated instructions</ExternalLink></figcaption>
  </figure>;
}
