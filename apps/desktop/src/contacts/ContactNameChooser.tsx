import { useEffect, useMemo, useRef, useState } from "react";
import { formatPhoneNumber, X } from "@peppy/desktop-ui";
import type { ContactBookView, DesktopBridge } from "../bridge/bridge";
import "./contact-name-chooser.css";

type ContactNameCandidates = Awaited<ReturnType<DesktopBridge["list_contact_name_candidates"]>>;
type ContactNameCandidate = ContactNameCandidates["candidates"][number];
type Props = {
  address: string;
  state: "conflict" | "pinned" | "consensus";
  sourceDeviceId?: string;
  books: ContactBookView[];
  bridge: Pick<DesktopBridge, "list_contact_name_candidates" | "set_contact_name_pin" | "clear_contact_name_pin">;
  onClose(): void;
  onRefresh(): void;
  onOpenContact(candidate: ContactNameCandidate): void;
};

const candidateKey = (candidate: { bookId: string; contactId: string }) => JSON.stringify([candidate.bookId, candidate.contactId]);

export function ContactNameChooser({ address, state, sourceDeviceId, books, bridge, onClose, onRefresh, onOpenContact }: Props) {
  const panelRef = useRef<HTMLDivElement>(null);
  const titleRef = useRef<HTMLHeadingElement>(null);
  const [result, setResult] = useState<ContactNameCandidates>();
  const [loadFailed, setLoadFailed] = useState(false);
  const [attempt, setAttempt] = useState(0);
  const [selected, setSelected] = useState<string>();
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState(false);
  const [status, setStatus] = useState("");
  const booksById = useMemo(() => new Map(books.map(book => [book.id, book])), [books]);

  useEffect(() => {
    let current = true;
    setResult(undefined);
    setLoadFailed(false);
    setSelected(undefined);
    void bridge.list_contact_name_candidates(address, sourceDeviceId).then(data => {
      if (!current) return;
      setResult(data);
      setSelected(data.pinned ? candidateKey(data.pinned) : undefined);
    }).catch(() => {
      if (current) setLoadFailed(true);
    });
    return () => { current = false; };
  }, [address, sourceDeviceId, bridge, attempt]);

  useEffect(() => {
    if (!result && !loadFailed) return;
    const radio = panelRef.current?.querySelector<HTMLInputElement>('input[type="radio"]:checked')
      ?? panelRef.current?.querySelector<HTMLInputElement>('input[type="radio"]');
    (radio ?? titleRef.current)?.focus();
  }, [result?.candidates, loadFailed]);

  useEffect(() => {
    const keydown = (event: KeyboardEvent) => {
      const focus = document.activeElement;
      if (event.key === "Escape" && !event.defaultPrevented
        && (focus === document.body || panelRef.current?.parentElement?.contains(focus))) {
        event.preventDefault();
        onClose();
      }
    };
    const pointerdown = (event: PointerEvent) => {
      if (event.target instanceof Node && !panelRef.current?.parentElement?.contains(event.target)) onClose();
    };
    document.addEventListener("keydown", keydown);
    document.addEventListener("pointerdown", pointerdown);
    return () => {
      document.removeEventListener("keydown", keydown);
      document.removeEventListener("pointerdown", pointerdown);
    };
  }, [onClose]);

  const candidate = result?.candidates.find(item => candidateKey(item) === selected);
  const remember = async () => {
    if (!candidate) return;
    setBusy(true);
    setError(false);
    try {
      await bridge.set_contact_name_pin(address, candidate.bookId, candidate.contactId, sourceDeviceId);
      setResult(current => current && { ...current, pinned: { bookId: candidate.bookId, contactId: candidate.contactId } });
      setStatus(`Using ${candidate.displayName} on this computer.`);
      onRefresh();
    } catch {
      setError(true);
    } finally {
      setBusy(false);
    }
  };
  const reset = async () => {
    setBusy(true);
    setError(false);
    try {
      await bridge.clear_contact_name_pin(address, sourceDeviceId);
      setResult(current => current && { ...current, pinned: null });
      setSelected(undefined);
      setStatus("Choice cleared. Choose a name again.");
      onRefresh();
    } catch {
      setError(true);
    } finally {
      setBusy(false);
    }
  };

  return <div id="contact-source-popover" className="contact-source-popover" role="dialog" aria-labelledby="contact-source-title" ref={panelRef}>
    <div className="contact-source-heading">
      <h2 id="contact-source-title" tabIndex={-1} ref={titleRef}>{state === "consensus" ? "Contact sources" : `Choose a name for ${formatPhoneNumber(address)}`}</h2>
      <button type="button" aria-label="Close contact sources" onClick={onClose}><X size={16} aria-hidden /></button>
    </div>
    {!result && !loadFailed && <p>Loading contacts…</p>}
    {(loadFailed || result?.moreIndexing) && <button type="button" onClick={() => setAttempt(current => current + 1)}>Try again</button>}
    {result && !result.moreIndexing && (result.candidates.length === 0 ? <p>No contacts available.</p> : <>
      <fieldset id="contact-source-options">
        <legend>Contact sources</legend>
        {result.candidates.map(item => <label data-contact-source={candidateKey(item)} key={candidateKey(item)}>
          <input type="radio" name="contact-source" checked={selected === candidateKey(item)} onChange={() => setSelected(candidateKey(item))} />
          <span className="contact-source-name">{item.displayName}</span>
          <span className="contact-source-book">{booksById.get(item.bookId)?.deviceName ?? "Contact book"} <span>ID {item.bookId.slice(-4)}</span></span>
        </label>)}
      </fieldset>
      <div id="contact-source-actions" className="contact-source-actions">
        <button type="button" data-action="remember-contact-name" disabled={!candidate || busy} onClick={() => void remember()}>Remember on this computer</button>
        <button type="button" data-action="open-source-contact" disabled={!candidate || busy} onClick={() => candidate && onOpenContact(candidate)}>Open contact</button>
        {result.pinned && <button type="button" data-action="reset-contact-name" disabled={busy} onClick={() => void reset()}>Reset choice</button>}
      </div>
    </>)}
    <p className="contact-source-hint">This computer only. Your phone and contacts don't change.</p>
    <p role="alert">{loadFailed ? "Couldn't load contacts. Try again." : error ? "Couldn't save this choice. Try again." : ""}</p>
    <p role="status">{result?.moreIndexing ? "Contacts are still being indexed. Try again in a moment." : status}</p>
  </div>;
}
