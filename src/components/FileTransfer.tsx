import { useEffect, useRef, useState } from "react";
import type { TransferProgress } from "../types";

type FileTransferProps = {
  onChooseFiles: () => void;
  onChooseFolder: () => void;
  onResume: () => void;
  progress: TransferProgress | null;
  busy: boolean;
  paired: boolean;
  canResume: boolean;
};

function formatBytes(bytes: number) {
  if (bytes < 1024) return `${bytes} B`;
  const units = ["KB", "MB", "GB", "TB"];
  let value = bytes / 1024;
  let unit = units[0];
  for (let index = 0; index < units.length && value >= 1024; index += 1) {
    unit = units[index + 1] ?? units[index];
    value /= 1024;
  }
  return `${value.toFixed(1)} ${unit}`;
}

export default function FileTransfer({ onChooseFiles, onChooseFolder, onResume, progress, busy, paired, canResume }: FileTransferProps) {
  const [throughput, setThroughput] = useState(0);
  const previousSample = useRef<{ transferId: string; bytes: number; time: number } | null>(null);

  useEffect(() => {
    if (!progress) {
      previousSample.current = null;
      setThroughput(0);
      return;
    }
    const now = performance.now();
    const previous = previousSample.current;
    if (previous?.transferId === progress.transferId && now > previous.time) {
      const byteDelta = progress.transferredBytes - previous.bytes;
      setThroughput(byteDelta >= 0 ? byteDelta / (now - previous.time) * 1000 / 1_048_576 : 0);
    } else {
      setThroughput(0);
    }
    previousSample.current = { transferId: progress.transferId, bytes: progress.transferredBytes, time: now };
  }, [progress]);

  const ratio = progress
    ? progress.totalBytes > 0
      ? progress.transferredBytes / progress.totalBytes
      : progress.completedEntries / Math.max(progress.totalEntries, 1)
    : 0;
  const percent = Math.min(100, Math.max(0, Math.round(ratio * 100)));

  return (
    <section className="file-transfer-panel">
      <div className="section-heading">
        <div>
          <p className="eyebrow">BULK CHANNEL / QUIC</p>
          <h2>Send files</h2>
        </div>
        <span className="step-index">8 MiB CHUNKS</span>
      </div>
      <p className="transfer-copy">Files and nested folders travel directly over your local network.</p>
      <div className="button-row transfer-actions">
        <button className="button button--accent" type="button" onClick={onChooseFiles} disabled={!paired || busy}>
          Choose files <span aria-hidden="true">↗</span>
        </button>
        <button className="button button--quiet" type="button" onClick={onChooseFolder} disabled={!paired || busy}>
          Choose folder
        </button>
      </div>
      {canResume && (
        <button className="text-button resume-button" type="button" onClick={onResume} disabled={busy || !paired}>
          Resume interrupted transfer <span aria-hidden="true">↻</span>
        </button>
      )}
      {!paired && <p className="transfer-hint">Scan a peer's pairing code before sending.</p>}
      {progress && (
        <div className="transfer-progress" role="status" aria-live="polite">
          <div className="progress-label">
            <span>{progress.direction === "sending" ? "SENDING" : "RECEIVING"} / {percent}%</span>
            <span>{throughput.toFixed(1)} MiB/s</span>
          </div>
          <div className="progress-track"><span style={{ width: `${percent}%` }} /></div>
          <p>{progress.currentPath} <span>{formatBytes(progress.transferredBytes)} / {formatBytes(progress.totalBytes)} · {progress.completedEntries}/{progress.totalEntries}</span></p>
        </div>
      )}
      <p className="destination-note">Incoming items save under Downloads / ZEROPASS.</p>
    </section>
  );
}