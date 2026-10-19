import { QRCodeSVG } from "qrcode.react";

type QRDisplayProps = { value: string };

export default function QRDisplay({ value }: QRDisplayProps) {
  return (
    <div className="qr-frame" aria-label="ZEROPASS pairing QR code">
      <QRCodeSVG
        value={value}
        size={208}
        level="Q"
        marginSize={2}
        bgColor="#f4f1e8"
        fgColor="#14201a"
      />
      <span className="qr-corner qr-corner--tl" />
      <span className="qr-corner qr-corner--tr" />
      <span className="qr-corner qr-corner--bl" />
      <span className="qr-corner qr-corner--br" />
    </div>
  );
}