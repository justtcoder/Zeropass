import { invoke } from "@tauri-apps/api/core";
import { listen } from "@tauri-apps/api/event";
import { writeText } from "@tauri-apps/plugin-clipboard-manager";
import { open } from "@tauri-apps/plugin-dialog";
import { useEffect, useState } from "react";
import FileTransfer from "./components/FileTransfer";
import QRDisplay from "./components/QRDisplay";
import QRScanner from "./components/QRScanner";
import QuickPad from "./components/QuickPad";
import type { PairingPayload, TransferProgress } from "./types";

type ResumeTransfer = { transferId: string; paths: string[] };

function createTransferId() {
  const bytes = crypto.getRandomValues(new Uint8Array(16));
  return Array.from(bytes, (byte) => byte.toString(16).padStart(2, "0")).join("");
}

function parsePayload(value: string): PairingPayload {
  const candidate = JSON.parse(value) as Partial<PairingPayload>;
  if (
    typeof candidate.ip !== "string" ||
    typeof candidate.port !== "number" ||
    typeof candidate.certificateDer !== "string" ||
    typeof candidate.token !== "string" ||
    typeof candidate.expiresInSeconds !== "number"
  ) {
    throw new Error("This is not a ZEROPASS pairing code.");
  }
  return candidate as PairingPayload;
}

export default function App() {
  const [localPayload, setLocalPayload] = useState<PairingPayload | null>(null);
  const [peerPayload, setPeerPayload] = useState<PairingPayload | null>(null);
  const [scannerOpen, setScannerOpen] = useState(false);
  const [busy, setBusy] = useState(false);
  const [fileBusy, setFileBusy] = useState(false);
  const [transferProgress, setTransferProgress] = useState<TransferProgress | null>(null);
  const [resumeTransfer, setResumeTransfer] = useState<ResumeTransfer | null>(null);
  const [notice, setNotice] = useState("");

  useEffect(() => {
    let active = true;
    const unlisten: Array<() => void> = [];
    listen<string>("clipboard-received", ({ payload }) => {
      void writeText(payload).catch((error: unknown) => {
        setNotice(`Clipboard write failed: ${String(error)}`);
      });
    }).then((stop) => { if (active) unlisten.push(stop); else stop(); })
      .catch((error: unknown) => setNotice(`Clipboard listener failed: ${String(error)}`));
    listen<TransferProgress>("file-transfer-progress", ({ payload }) => setTransferProgress(payload))
      .then((stop) => { if (active) unlisten.push(stop); else stop(); })
      .catch((error: unknown) => setNotice(`Transfer listener failed: ${String(error)}`));
    listen<{ savedTo: string; files: number; totalBytes: number }>("file-transfer-received", ({ payload }) => {
      setNotice(`Received ${payload.files} files. Saved to ${payload.savedTo}`);
    }).then((stop) => { if (active) unlisten.push(stop); else stop(); })
      .catch((error: unknown) => setNotice(`Receive listener failed: ${String(error)}`));
    listen<string>("file-transfer-error", ({ payload }) => setNotice(`Incoming transfer failed: ${payload}`))
      .then((stop) => { if (active) unlisten.push(stop); else stop(); })
      .catch((error: unknown) => setNotice(`Transfer error listener failed: ${String(error)}`));
    return () => {
      active = false;
      unlisten.forEach((stop) => stop());
    };
  }, []);

  async function createPairing() {
    setNotice("");
    try {
      const payload = await invoke<PairingPayload>("start_pairing");
      setLocalPayload(payload);
      setNotice("Pairing code is live for five minutes.");
    } catch (error) {
      setNotice(`Could not start local listener: ${String(error)}`);
    }
  }

  async function copyPairingCode() {
    if (!localPayload) return;
    try {
      await writeText(JSON.stringify(localPayload));
      setNotice("Pairing code copied.");
    } catch (error) {
      setNotice(`Could not copy pairing code: ${String(error)}`);
    }
  }

  async function acceptScan(value: string) {
    try {
      const payload = parsePayload(value);
      setNotice("Authenticating pinned QUIC peer…");
      await invoke("connect_peer", { payload });
      setPeerPayload(payload);
      setNotice("Peer authenticated. Pairing code expires in five minutes.");
      setScannerOpen(false);
    } catch (error) {
      setNotice(error instanceof Error ? error.message : "Could not read pairing code.");
    }
  }

  async function sendToPeer(text: string) {
    if (!peerPayload) return;
    setBusy(true);
    setNotice("");
    try {
      await invoke("send_clipboard", { payload: peerPayload, text });
      setNotice("Text delivered to the paired device.");
    } catch (error) {
      setNotice(`Send failed: ${String(error)}`);
      throw error;
    } finally {
      setBusy(false);
    }
  }

  async function copyLocally(text: string) {
    try {
      if ((window as Window & { __TAURI_INTERNALS__?: unknown }).__TAURI_INTERNALS__) {
        await writeText(text);
      } else {
        await navigator.clipboard.writeText(text);
      }
    } catch (error) {
      setNotice(`Clipboard write failed: ${String(error)}`);
      throw error;
    }
  }

  async function choosePaths(directory: boolean) {
    if (!peerPayload) return;
    try {
      const selection = await open({
        title: directory ? "Choose a folder to send" : "Choose files to send",
        multiple: !directory,
        directory,
      });
      if (!selection) return;
      const paths = Array.isArray(selection) ? selection : [selection];
      const transfer = { transferId: createTransferId(), paths };
      setResumeTransfer(transfer);
      await runFileTransfer(transfer);
    } catch (error) {
      setNotice(`File transfer failed: ${String(error)}`);
    }
  }

  async function runFileTransfer(transfer: ResumeTransfer) {
    if (!peerPayload) return;
    setFileBusy(true);
    setTransferProgress(null);
    setNotice("Preparing files for resumable QUIC transfer…");
    try {
      const summary = await invoke<{ files: number; totalBytes: number }>("send_files", {
        payload: peerPayload,
        paths: transfer.paths,
        transferId: transfer.transferId,
      });
      setNotice(`Sent ${summary.files} files (${(summary.totalBytes / 1_048_576).toFixed(1)} MiB).`);
      setResumeTransfer(null);
    } catch (error) {
      setNotice(`Transfer paused. Retry to resume verified chunks: ${String(error)}`);
    } finally {
      setFileBusy(false);
    }
  }
  return (
    <main className="app-shell min-h-screen">
      <header className="topbar">
        <a className="wordmark" href="#home" aria-label="ZEROPASS home">
          <span className="wordmark-mark">Z</span>
          <span>ZERO<span className="wordmark-light">PASS</span></span>
        </a>
        <div className="topbar-right">
          <span className="network-state"><i /> LOCAL NETWORK</span>
          <span className="version-label">ZEROPASS / 0.1.0</span>
        </div>
      </header>

      <section className="intro" id="home">
        <div>
          <p className="eyebrow">PRIVATE TRANSFER / NO CLOUD</p>
          <h1>Your devices.<br /><em>One local link.</em></h1>
        </div>
        <p className="intro-copy">A direct, encrypted path for text and files on the same network. Your pairing code stays on this screen.</p>
      </section>

      <div className="dashboard-grid">
        <section className="pair-panel">
          <div className="section-heading">
            <div>
              <p className="eyebrow">01 / PAIR A DEVICE</p>
              <h2>Scan to connect</h2>
            </div>
            <span className="step-index">A—01</span>
          </div>

          <div className="qr-stage">
            {localPayload ? (
              <QRDisplay value={JSON.stringify(localPayload)} />
            ) : (
              <div className="qr-empty"><span className="qr-cross">+</span><span>PAIRING CODE<br />NOT ACTIVE</span></div>
            )}
          </div>

          {localPayload ? (
            <div className="endpoint-line"><span className="endpoint-dot" />{localPayload.ip}:{localPayload.port}<span className="endpoint-timer">5 MIN</span></div>
          ) : (
            <p className="muted-copy">Generate a short-lived local session to share this device.</p>
          )}

          <div className="button-row pairing-actions">
            <button className="button button--accent" type="button" onClick={createPairing}>
              {localPayload ? "Refresh code" : "Create pairing code"}<span aria-hidden="true">↗</span>
            </button>
            {localPayload && (
              <button className="button button--quiet" type="button" onClick={() => void copyPairingCode()}>
                Copy code
              </button>
            )}
            <button className="button button--quiet" type="button" onClick={() => setScannerOpen(true)}>
              Scan a code
            </button>
          </div>
        </section>

        <div className="right-column">
          <section className="link-panel">
            <div className="section-heading">
              <div>
                <p className="eyebrow">02 / CONNECTION</p>
                <h2>Link status</h2>
              </div>
              <span className={`status-pill ${peerPayload ? "status-pill--connected" : ""}`}>
                 <i />{peerPayload ? "TRUSTED" : "STANDBY"}
              </span>
            </div>
            <div className="link-details">
              <div><span>PEER ADDRESS</span><strong>{peerPayload ? `${peerPayload.ip}:${peerPayload.port}` : "No device linked"}</strong></div>
              <div><span>TRANSPORT</span><strong>QUIC / TLS 1.3</strong></div>
              <div><span>TRUST MODEL</span><strong>QR token + pinned cert</strong></div>
            </div>
            {peerPayload && <button className="text-button" type="button" onClick={() => setPeerPayload(null)}>Forget paired code <span aria-hidden="true">×</span></button>}
          </section>

          <QuickPad onSend={sendToPeer} onCopy={copyLocally} busy={busy} paired={Boolean(peerPayload)} />
          <FileTransfer
            onChooseFiles={() => void choosePaths(false)}
            onChooseFolder={() => void choosePaths(true)}
            onResume={() => resumeTransfer && void runFileTransfer(resumeTransfer)}
            progress={transferProgress}
            busy={fileBusy}
            paired={Boolean(peerPayload)}
            canResume={Boolean(resumeTransfer)}
          />
        </div>
      </div>

      <footer className="footer-line">
        <span>ZEROPASS / LOCAL DATA HIGHWAY</span>
        <span>TRAFFIC REMAINS ON YOUR LAN</span>
      </footer>

      {notice && <div className="toast" role="status">{notice}</div>}
      {scannerOpen && <QRScanner onScan={acceptScan} onClose={() => setScannerOpen(false)} />}
    </main>
  );
}