import { useState } from "react";

type QuickPadProps = {
  onSend: (text: string) => Promise<void>;
  onCopy: (text: string) => Promise<void>;
  busy: boolean;
  paired: boolean;
};

export default function QuickPad({ onSend, onCopy, busy, paired }: QuickPadProps) {
  const [text, setText] = useState("");
  const [status, setStatus] = useState("");

  async function copyText() {
    if (!text.trim()) return;
    await onCopy(text);
    setStatus("Copied to this device");
  }

  async function sendText() {
    if (!text.trim()) return;
    await onSend(text);
    setStatus("Delivered over local QUIC");
  }

  return (
    <section className="quickpad-panel">
      <div className="section-heading">
        <div>
          <p className="eyebrow">CONTROL CHANNEL / TEXT</p>
          <h2>Quick pad</h2>
        </div>
        <span className="live-indicator">{paired ? "PEER TRUSTED" : "LOCAL ONLY"}</span>
      </div>
      <textarea
        value={text}
        onChange={(event) => { setText(event.target.value); setStatus(""); }}
        maxLength={1000000}
        placeholder="Drop a note, command, or link…"
        aria-label="Quick pad text"
      />
      <div className="pad-footer">
        <span className="status-copy" aria-live="polite">{status || `${text.length.toLocaleString()} / 1,000,000`}</span>
        <div className="button-row">
          <button className="button button--quiet" type="button" onClick={copyText} disabled={!text.trim()}>
            Copy here
          </button>
          <button className="button button--accent" type="button" onClick={sendText} disabled={!paired || !text.trim() || busy}>
            {busy ? "Sending…" : "Send to peer"}
            <span aria-hidden="true">↗</span>
          </button>
        </div>
      </div>
    </section>
  );
}