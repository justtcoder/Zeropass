export type PairingPayload = {
  ip: string;
  port: number;
  certificateDer: string;
  token: string;
  expiresInSeconds: number;
};

export type TransferProgress = {
  transferId: string;
  direction: "sending" | "receiving";
  currentPath: string;
  transferredBytes: number;
  totalBytes: number;
  completedEntries: number;
  totalEntries: number;
};