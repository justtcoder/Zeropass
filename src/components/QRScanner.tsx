import { useEffect, useRef, useState } from "react";

type QRScannerProps = {
  onScan: (value: string) => void;
  onClose: () => void;
};

export default function QRScanner({ onScan, onClose }: QRScannerProps) {
  const videoRef = useRef<HTMLVideoElement>(null);
  const onScanRef = useRef(onScan);
  const [error, setError] = useState("");
  const [pairingCode, setPairingCode] = useState("");

  onScanRef.current = onScan;

  useEffect(() => {
    let active = true;
    let stop: (() => void) | undefined;
    let scanned = false;

    import("@zxing/browser").then(({ BrowserQRCodeReader }) => {
      if (!active) return;
      return new BrowserQRCodeReader().decodeFromVideoDevice(undefined, videoRef.current!, (result) => {
        if (result && !scanned) {
          scanned = true;
          stop?.();
          onScanRef.current(result.getText());
        }
      }).then((controls) => {
        if (active) stop = () => controls.stop();
        else controls.stop();
      });
    }).catch((scanError: unknown) => {
      setError(scanError instanceof Error ? scanError.message : "Camera is unavailable");
    });

    return () => {
      active = false;
      stop?.();
    };
  }, []);

  return (
    <div className="scanner-scrim" role="presentation" onClick={onClose}>
      <section
        className="scanner-dialog"
        role="dialog"
        aria-modal="true"
        aria-labelledby="scanner-title"
        onClick={(event) => event.stopPropagation()}
      >
        <div className="section-heading">
          <div>
            <p className="eyebrow">DEVICE LINK</p>
            <h2 id="scanner-title">Scan pairing code</h2>
          </div>
          <button className="icon-button" type="button" onClick={onClose} aria-label="Close scanner">×</button>
        </div>
        <div className="camera-view">
          <video ref={videoRef} muted playsInline />
          <span className="scan-reticle" />
        </div>
        <p className="scanner-status">{error || "Point the camera at the code on your other device."}</p>
        <form
          className="pairing-code-form"
          onSubmit={(event) => {
            event.preventDefault();
            const value = pairingCode.trim();
            if (value) onScan(value);
          }}
        >
          <label htmlFor="pairing-code">Or paste a pairing code</label>
          <textarea
            id="pairing-code"
            value={pairingCode}
            onChange={(event) => setPairingCode(event.target.value)}
            autoCapitalize="none"
            autoComplete="off"
            spellCheck={false}
            rows={3}
          />
          <button className="button button--accent" type="submit" disabled={!pairingCode.trim()}>
            Connect with code
          </button>
        </form>
      </section>
    </div>
  );
}