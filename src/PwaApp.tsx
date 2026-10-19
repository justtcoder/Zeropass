import { useEffect, useRef, useState } from "react";
import { QRCodeSVG } from "qrcode.react";
import appIcon from "../src-tauri/icons/icon.png";
import QRScanner from "./components/QRScanner";
import "./pwa.css";

type SignalCode = {
  version: 1;
  type: "offer" | "answer";
  sdp: string;
};

type IncomingFile = {
  name: string;
  type: string;
  size: number;
  received: number;
  chunks: BlobPart[];
};

type TransferRequest = { name: string; type: string; size: number };
type DownloadFile = { name: string; url: string; size: number };
type InstallPromptEvent = Event & {
  prompt: () => Promise<void>;
  userChoice: Promise<{ outcome: "accepted" | "dismissed" }>;
};

const CHUNK_BYTES = 32 * 1024;
const CHANNEL_HIGH_WATER_BYTES = 1024 * 1024;
const MAX_FILE_BYTES = 64 * 1024 * 1024;

function waitForIceGathering(connection: RTCPeerConnection) {
  if (connection.iceGatheringState === "complete") return Promise.resolve();
  return new Promise<void>((resolve) => {
    const finish = () => {
      window.clearTimeout(timeout);
      connection.removeEventListener("icegatheringstatechange", checkState);
      resolve();
    };
    const checkState = () => {
      if (connection.iceGatheringState === "complete") finish();
    };
    const timeout = window.setTimeout(finish, 12000);
    connection.addEventListener("icegatheringstatechange", checkState);
  });
}

function waitForBufferedAmount(channel: RTCDataChannel) {
  if (channel.bufferedAmount <= CHANNEL_HIGH_WATER_BYTES) return Promise.resolve();
  return new Promise<void>((resolve) => {
    const checkAmount = () => {
      if (channel.bufferedAmount <= CHANNEL_HIGH_WATER_BYTES / 4) {
        channel.removeEventListener("bufferedamountlow", checkAmount);
        resolve();
      }
    };
    channel.addEventListener("bufferedamountlow", checkAmount);
    checkAmount();
  });
}

function parseSignal(value: string): SignalCode {
  const signal = JSON.parse(value) as Partial<SignalCode>;
  if (
    signal.version !== 1 ||
    (signal.type !== "offer" && signal.type !== "answer") ||
    typeof signal.sdp !== "string"
  ) {
    throw new Error("That is not a ZEROPASS browser pairing code.");
  }
  return signal as SignalCode;
}

function signalLink(signalText: string) {
  const bytes = new TextEncoder().encode(signalText);
  let binary = "";
  for (const byte of bytes) binary += String.fromCharCode(byte);
  const encoded = btoa(binary).replaceAll("+", "-").replaceAll("/", "_").replace(/=+$/, "");
  const url = new URL(import.meta.env.VITE_PUBLIC_APP_URL || window.location.href);
  url.search = "";
  url.hash = new URLSearchParams({ join: encoded }).toString();
  return url.toString();
}

function signalFromLink(value: string) {
  try {
    const url = new URL(value);
    const encoded = new URLSearchParams(url.hash.slice(1)).get("join");
    if (encoded) {
      const base64 = encoded.replaceAll("-", "+").replaceAll("_", "/");
      const binary = atob(base64.padEnd(Math.ceil(base64.length / 4) * 4, "="));
      const bytes = Uint8Array.from(binary, (character) => character.charCodeAt(0));
      return new TextDecoder().decode(bytes);
    }
  } catch {
    return value;
  }
  return value;
}

export default function PwaApp() {
  const connectionRef = useRef<RTCPeerConnection | null>(null);
  const channelRef = useRef<RTCDataChannel | null>(null);
  const roleRef = useRef<"offerer" | "answerer" | null>(null);
  const incomingRef = useRef<IncomingFile | null>(null);
  const senderDecisionRef = useRef<((accepted: boolean) => void) | null>(null);
  const downloadUrlRef = useRef("");
  const [localCode, setLocalCode] = useState("");
  const [scannerOpen, setScannerOpen] = useState(false);
  const [connected, setConnected] = useState(false);
  const [busy, setBusy] = useState(false);
  const [status, setStatus] = useState("Not connected");
  const [progress, setProgress] = useState<{ transferred: number; total: number } | null>(null);
  const [selectedFile, setSelectedFile] = useState<File | null>(null);
  const [receivedFile, setReceivedFile] = useState<DownloadFile | null>(null);
  const [transferRequest, setTransferRequest] = useState<TransferRequest | null>(null);
  const [flow, setFlow] = useState<"idle" | "send" | "receive">("idle");
  const [installPrompt, setInstallPrompt] = useState<InstallPromptEvent | null>(null);
  const fileInputRef = useRef<HTMLInputElement>(null);

  useEffect(() => {
    const captureInstallPrompt = (event: Event) => {
      event.preventDefault();
      setInstallPrompt(event as InstallPromptEvent);
    };
    window.addEventListener("beforeinstallprompt", captureInstallPrompt);
    return () => window.removeEventListener("beforeinstallprompt", captureInstallPrompt);
  }, []);

  useEffect(() => () => {
    connectionRef.current?.close();
    if (downloadUrlRef.current) URL.revokeObjectURL(downloadUrlRef.current);
  }, []);

  useEffect(() => {
    const encoded = new URLSearchParams(window.location.hash.slice(1)).get("join");
    if (!encoded) return;
    window.history.replaceState(null, "", `${window.location.pathname}${window.location.search}`);
    const decoded = signalFromLink(`${window.location.origin}${window.location.pathname}#join=${encoded}`);
    void acceptCode(decoded);
  }, []);

  function attachChannel(channel: RTCDataChannel) {
    channelRef.current = channel;
    channel.binaryType = "arraybuffer";
    channel.bufferedAmountLowThreshold = CHANNEL_HIGH_WATER_BYTES / 4;
    channel.onopen = () => {
      setConnected(true);
      setLocalCode("");
      setStatus("Encrypted peer link connected. Incoming files still need your approval.");
    };
    channel.onclose = () => {
      setConnected(false);
      senderDecisionRef.current?.(false);
      senderDecisionRef.current = null;
      setStatus("Peer connection closed.");
    };
    channel.onerror = () => setStatus("Peer data channel failed.");
    channel.onmessage = (event: MessageEvent<unknown>) => {
      void receiveData(event.data);
    };
  }

  async function receiveData(data: unknown) {
    if (typeof data === "string") {
      let message: { type?: string; name?: string; mimeType?: string; size?: number };
      try {
        message = JSON.parse(data) as typeof message;
      } catch {
        setStatus("Received an invalid transfer message.");
        return;
      }
      if (message.type === "file-accept" || message.type === "file-reject") {
        senderDecisionRef.current?.(message.type === "file-accept");
        senderDecisionRef.current = null;
        return;
      }
      if (message.type === "file-start" && typeof message.name === "string" && typeof message.size === "number" && Number.isSafeInteger(message.size) && message.size >= 0) {
        if (message.size > MAX_FILE_BYTES) {
          incomingRef.current = null;
          channelRef.current?.send(JSON.stringify({ type: "file-reject" }));
          setStatus("This browser proof of concept accepts files up to 64 MiB.");
          return;
        }
        setTransferRequest({
          name: message.name,
          type: typeof message.mimeType === "string" ? message.mimeType : "application/octet-stream",
          size: message.size,
        });
        setReceivedFile(null);
        setStatus("Review the incoming file and approve it to receive.");
        return;
      }
      if (message.type === "file-complete") {
        const incoming = incomingRef.current;
        incomingRef.current = null;
        if (!incoming || incoming.received !== incoming.size) {
          setStatus("Received file size did not match the sender’s announcement.");
          return;
        }
        const blob = new Blob(incoming.chunks, { type: incoming.type });
        if (downloadUrlRef.current) URL.revokeObjectURL(downloadUrlRef.current);
        const url = URL.createObjectURL(blob);
        downloadUrlRef.current = url;
        setReceivedFile({ name: incoming.name, url, size: incoming.size });
        setStatus(`Received ${incoming.name}. Save it to this device.`);
      }
      return;
    }

    const incoming = incomingRef.current;
    if (!incoming) return;
    const chunk = data instanceof ArrayBuffer
      ? data
      : data instanceof Blob
        ? await data.arrayBuffer()
        : null;
    if (!chunk) return;
    if (chunk.byteLength > incoming.size - incoming.received) {
      incomingRef.current = null;
      setStatus("Received more data than the announced file size; transfer stopped.");
      return;
    }
    incoming.chunks.push(chunk);
    incoming.received += chunk.byteLength;
    setProgress({ transferred: incoming.received, total: incoming.size });
  }

  function newConnection() {
    connectionRef.current?.close();
    channelRef.current = null;
    incomingRef.current = null;
    setConnected(false);
    setProgress(null);
    setReceivedFile(null);
    if (downloadUrlRef.current) URL.revokeObjectURL(downloadUrlRef.current);
    downloadUrlRef.current = "";

    const connection = new RTCPeerConnection({ iceServers: [] });
    connection.onconnectionstatechange = () => {
      if (connection.connectionState === "failed") {
        setStatus("Direct connection failed. Check that both devices share a reachable network.");
      }
    };
    connectionRef.current = connection;
    return connection;
  }

  async function createOffer() {
    try {
      const connection = newConnection();
      roleRef.current = "offerer";
      const channel = connection.createDataChannel("zeropass-file", { ordered: true });
      attachChannel(channel);
      await connection.setLocalDescription(await connection.createOffer());
      setStatus("Gathering local connection details…");
      await waitForIceGathering(connection);
      const sdp = connection.localDescription?.sdp;
      if (!sdp) throw new Error("Could not create a browser pairing offer.");
      setLocalCode(JSON.stringify({ version: 1, type: "offer", sdp } satisfies SignalCode));
      setStatus("Show this pairing QR to the receiving device.");
    } catch (error) {
      setStatus(`Could not create pairing offer: ${String(error)}`);
    }
  }

  async function acceptCode(value: string) {
    try {
      const signal = parseSignal(signalFromLink(value.trim()));
      if (signal.type === "answer") {
        const connection = connectionRef.current;
        if (!connection || roleRef.current !== "offerer") {
          throw new Error("Create an offer on this device before entering an answer.");
        }
        await connection.setRemoteDescription({ type: "answer", sdp: signal.sdp });
        setLocalCode("");
        setFlow("send");
        setStatus("Connecting securely…");
        return;
      }

      const connection = newConnection();
      roleRef.current = "answerer";
      setFlow("receive");
      connection.ondatachannel = (event) => attachChannel(event.channel);
      await connection.setRemoteDescription({ type: "offer", sdp: signal.sdp });
      await connection.setLocalDescription(await connection.createAnswer());
      setStatus("Preparing the answer code…");
      await waitForIceGathering(connection);
      const sdp = connection.localDescription?.sdp;
      if (!sdp) throw new Error("Could not create a browser pairing answer.");
      setLocalCode(JSON.stringify({ version: 1, type: "answer", sdp } satisfies SignalCode));
      setStatus("Show this pairing QR to the sending device.");
    } catch (error) {
      setStatus(error instanceof Error ? error.message : `Pairing failed: ${String(error)}`);
    }
  }

  function startSend() {
    setFlow("send");
    setSelectedFile(null);
    if (fileInputRef.current) {
      fileInputRef.current.value = "";
      fileInputRef.current.click();
    }
  }

  function startReceive() {
    setFlow("receive");
    setLocalCode("");
    setStatus("Scan the sender’s pairing QR code.");
    setScannerOpen(true);
  }

  function scanReply() {
    setScannerOpen(true);
    setStatus("Scan the receiver’s confirmation QR code.");
  }

  async function sendFile() {
    const channel = channelRef.current;
    if (!channel || channel.readyState !== "open" || !selectedFile) return;
    if (selectedFile.size > MAX_FILE_BYTES) {
      setStatus("This browser proof of concept accepts files up to 64 MiB.");
      return;
    }
    setBusy(true);
    setProgress({ transferred: 0, total: selectedFile.size });
    try {
      channel.send(JSON.stringify({
        type: "file-start",
        name: selectedFile.name,
        mimeType: selectedFile.type,
        size: selectedFile.size,
      }));
      setStatus("Waiting for the receiver to approve this file…");
      const accepted = await new Promise<boolean>((resolve) => {
        const timeout = window.setTimeout(() => {
          senderDecisionRef.current = null;
          resolve(false);
        }, 120000);
        senderDecisionRef.current = (decision) => {
          window.clearTimeout(timeout);
          resolve(decision);
        };
      });
      if (!accepted) {
        setStatus("The receiver declined or did not approve the file in time.");
        return;
      }
      for (let offset = 0; offset < selectedFile.size; offset += CHUNK_BYTES) {
        await waitForBufferedAmount(channel);
        const chunk = await selectedFile.slice(offset, offset + CHUNK_BYTES).arrayBuffer();
        channel.send(chunk);
        setProgress({ transferred: Math.min(offset + chunk.byteLength, selectedFile.size), total: selectedFile.size });
      }
      channel.send(JSON.stringify({ type: "file-complete" }));
      setStatus(`Sent ${selectedFile.name}.`);
    } catch (error) {
      setStatus(`File send failed: ${String(error)}`);
    } finally {
      setBusy(false);
    }
  }

  function authorizeTransfer() {
    if (!transferRequest || channelRef.current?.readyState !== "open") return;
    incomingRef.current = {
      ...transferRequest,
      received: 0,
      chunks: [],
    };
    channelRef.current.send(JSON.stringify({ type: "file-accept" }));
    setProgress({ transferred: 0, total: transferRequest.size });
    setStatus(`Receiving ${transferRequest.name}…`);
    setTransferRequest(null);
  }

  function declineTransfer() {
    if (channelRef.current?.readyState === "open") {
      channelRef.current.send(JSON.stringify({ type: "file-reject" }));
    }
    setTransferRequest(null);
    setStatus("File declined. The sender was notified.");
  }

  async function installPwa() {
    if (!installPrompt) return;
    await installPrompt.prompt();
    await installPrompt.userChoice;
    setInstallPrompt(null);
  }

  const signalType = localCode ? parseSignal(localCode).type : null;
  const localQrLink = localCode ? signalLink(localCode) : "";
  const iosInstallHint = /iphone|ipad|ipod/i.test(navigator.userAgent);

  return (
    <main className="app-shell pwa-shell min-h-screen">
      <header className="topbar">
        <a className="wordmark" href="#home" aria-label="ZEROPASS home">
          <img className="wordmark-mark" src={appIcon} alt="" />
          <span>ZERO<span className="wordmark-light">PASS</span></span>
        </a>
        <div className="topbar-right">
          <span className="network-state"><i /> BROWSER POC</span>
          {installPrompt && <button className="button button--quiet pwa-install" type="button" onClick={() => void installPwa()}>Install app</button>}
        </div>
      </header>

      <section className="intro" id="home">
        <div>
          <p className="eyebrow">PRIVATE / LOCAL NETWORK</p>
          <h1>Send and receive.<br /><em>Nothing to decode.</em></h1>
        </div>
        <p className="intro-copy">Choose an action. Pair by scanning the other device’s QR, then approve every incoming file before it is transferred.</p>
      </section>

      <details className="pwa-help">
        <summary>How to connect and share files</summary>
        <ol>
          <li><strong>Sender:</strong> tap Send, choose one file, then show the pairing QR.</li>
          <li><strong>Receiver:</strong> scan that QR with the phone camera or Google Lens. It opens ZEROPASS and displays a confirmation QR.</li>
          <li><strong>Sender:</strong> tap Scan receiver QR and scan the confirmation.</li>
          <li><strong>Receiver:</strong> check the file name and size, then tap Authorize &amp; receive. Save the file when it arrives.</li>
        </ol>
        <p>Both devices need a reachable local network. Keep ZEROPASS open during the transfer. These QR links work with the browser PWA; desktop QUIC codes need the desktop app scanner.</p>
      </details>

      <input
        ref={fileInputRef}
        className="pwa-hidden-file"
        type="file"
        onChange={(event) => {
          const file = event.currentTarget.files?.[0] ?? null;
          if (!file) return;
          setSelectedFile(file);
          setStatus(`Ready to send ${file.name}. Pair with the receiving device.`);
          void createOffer();
        }}
      />

      {!connected && !localCode && (
        <section className="pwa-actions" aria-label="Transfer actions">
          <button className="pwa-action pwa-action--send" type="button" onClick={startSend}>
            <span className="pwa-action-icon" aria-hidden="true">↑</span>
            <span className="pwa-action-title">Send</span>
            <span className="pwa-action-copy">Choose a file to share</span>
          </button>
          <button className="pwa-action pwa-action--receive" type="button" onClick={startReceive}>
            <span className="pwa-action-icon" aria-hidden="true">↓</span>
            <span className="pwa-action-title">Receive</span>
            <span className="pwa-action-copy">Scan the sender’s QR</span>
          </button>
        </section>
      )}

      {!connected && localCode && (
        <section className="pair-panel pwa-pair-panel">
          <div className="section-heading">
            <div><p className="eyebrow">{signalType === "answer" ? "CONFIRM CONNECTION" : "CONNECT TO RECEIVER"}</p><h2>{signalType === "answer" ? "Scan this QR on the sender’s device" : "Show this QR to the receiver"}</h2></div>
            <span className="step-index">{flow === "send" ? "SEND" : "RECEIVE"}</span>
          </div>
          <div className="pwa-qr-stage">
            {localQrLink.length <= 2800 ? (
              <div className="qr-frame pwa-qr" aria-label="Secure device pairing QR code">
                <QRCodeSVG value={localQrLink} size={320} level="L" marginSize={2} bgColor="#f4f1e8" fgColor="#14201a" />
                <span className="qr-corner qr-corner--tl" /><span className="qr-corner qr-corner--tr" />
                <span className="qr-corner qr-corner--bl" /><span className="qr-corner qr-corner--br" />
              </div>
            ) : (
              <div className="qr-empty"><span className="qr-cross">!</span><span>QR code could not be created.<br />Start again on this device.</span></div>
            )}
          </div>
          <p className="pwa-status" role="status">{status}</p>
          <div className="button-row pairing-actions">
            {flow === "send" && <button className="button button--accent" type="button" onClick={scanReply}>Scan receiver QR</button>}
            <button className="button button--quiet" type="button" onClick={() => { connectionRef.current?.close(); connectionRef.current = null; channelRef.current = null; roleRef.current = null; setLocalCode(""); setFlow("idle"); setSelectedFile(null); setStatus("Not connected"); }}>Cancel</button>
          </div>
        </section>
      )}

      {connected && (
        <div className="pwa-transfer-layout">
          <section className="link-panel pwa-transfer-panel">
            <div className="section-heading">
              <div><p className="eyebrow">ENCRYPTED CONNECTION</p><h2>{flow === "send" ? "Ready to send" : "Ready to receive"}</h2></div>
              <span className="status-pill status-pill--connected"><i />CONNECTED</span>
            </div>
            <p className="pwa-status" role="status">{status}</p>
            {flow === "send" && selectedFile && (
              <div className="pwa-selected-file">
                <span>{selectedFile.name}</span><small>{(selectedFile.size / 1_048_576).toFixed(1)} MiB</small>
              </div>
            )}
            {flow === "send" && (
              <button className="button button--accent" type="button" disabled={!selectedFile || busy} onClick={() => void sendFile()}>
                {busy ? "Waiting for approval…" : "Send file"}
              </button>
            )}
            {progress && (
              <div className="transfer-progress">
                <div className="progress-label"><span>{receivedFile ? "Received" : "Progress"}</span><span>{progress.total ? `${Math.round(progress.transferred / progress.total * 100)}%` : "100%"}</span></div>
                <div className="progress-track"><span style={{ width: `${progress.total ? Math.min(100, progress.transferred / progress.total * 100) : 100}%` }} /></div>
              </div>
            )}
            {receivedFile && (
              <a className="button button--quiet pwa-download" href={receivedFile.url} download={receivedFile.name}>
                Save received file
              </a>
            )}
            {iosInstallHint && <p className="transfer-hint">Install: Safari Share menu, then Add to Home Screen.</p>}
          </section>

          {transferRequest && (
            <section className="pwa-authorize" role="dialog" aria-modal="true" aria-labelledby="authorize-title">
              <p className="eyebrow">INCOMING FILE</p>
              <h2 id="authorize-title">Approve this transfer?</h2>
              <p className="pwa-request-name">{transferRequest.name}</p>
              <p className="pwa-request-meta">{(transferRequest.size / 1_048_576).toFixed(1)} MiB · {transferRequest.type || "Unknown file type"}</p>
              <p className="transfer-hint">The file is not received until you approve it. The sender’s identity is not verified; only approve a transfer you expect.</p>
              <div className="button-row">
                <button className="button button--accent" type="button" onClick={authorizeTransfer}>Authorize & receive</button>
                <button className="button button--quiet" type="button" onClick={declineTransfer}>Decline</button>
              </div>
            </section>
          )}
        </div>
      )}

      {!connected && flow === "send" && !localCode && <p className="pwa-status" role="status">{status}</p>}

      <footer className="footer-line"><span>ZEROPASS / PRIVATE FILE TRANSFER</span><span>{connected ? "END-TO-END ENCRYPTED" : "SAME NETWORK REQUIRED"}</span></footer>
      {scannerOpen && <QRScanner showManualEntry={false} onScan={(value) => { setScannerOpen(false); void acceptCode(value); }} onClose={() => setScannerOpen(false)} />}
    </main>
  );
}