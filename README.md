# ZEROPASS

ZEROPASS is a Tauri v2 + React local-transfer starter. It creates a short-lived QUIC listener, publishes its address, TLS certificate and bearer token as a QR code, verifies a scanned peer with the pinned certificate, and transfers clipboard text plus files/folders over QUIC.

## Project map

```text
zeropass/
├── package.json
├── package-lock.json
├── index.html
├── vite.config.ts
├── tsconfig.json
├── src/
│   ├── App.tsx
│   ├── main.tsx
│   ├── styles.css
│   ├── types.ts
│   └── components/
│       ├── QRDisplay.tsx
│       ├── QRScanner.tsx
│       ├── FileTransfer.tsx
│       └── QuickPad.tsx
└── src-tauri/
	├── Cargo.toml
	├── build.rs
	├── tauri.conf.json
	├── capabilities/default.json
	└── src/
		├── lib.rs
		├── main.rs
		└── engine/
			├── mod.rs
			├── quic_engine.rs
			├── file_transfer.rs
			├── handshake.rs
			├── clipboard.rs
			├── swarm.rs
			└── auth_bridge.rs
```

`tauri android init` and `tauri ios init` generate platform project directories. Add Android `CAMERA` permission and iOS `NSCameraUsageDescription` to those generated projects before shipping camera scanning.

## Run

Install Rust with rustup, Node.js LTS, and the native prerequisites listed in the Tauri v2 setup guide for your OS. Then run:

```sh
npm install
npm run tauri -- dev
```

## Desktop builds

The `Desktop Builds` GitHub Actions workflow builds Windows and macOS bundles on pull requests, pushes to `main`, version tags, and manual runs. Download the unsigned `.msi`/`.exe` or `.dmg` from the workflow run's artifacts. These builds are for testing; Windows SmartScreen and macOS Gatekeeper may warn because code signing and notarization are not configured yet.

On the receiving device, create a pairing code. Scan it from the sending device, then use Quick Pad or choose files/folder. Incoming items are written beneath `Downloads/ZEROPASS/<transfer-id>` without overwriting existing paths. Both devices must be reachable over the same LAN; firewalls and guest Wi-Fi isolation can block UDP.

## Security and performance boundaries

- Pairing uses TLS 1.3 with a self-signed certificate pinned from the QR payload and a random 256-bit bearer token. The listener closes after five minutes. A QR code is a credential: only display it to a trusted peer.
- LAN address selection uses `if-addrs`; `mdns-sd` is included for discovery integration, but service advertisement and browsing are not wired yet. The current flow uses the address encoded in the QR payload.
- Tauri capabilities authorize frontend plugin commands. They do not grant raw socket access; the Rust process binds QUIC directly. OS firewall policy still applies.
- File/folder transfers pipeline up to four entries concurrently over QUIC, use mmap-backed 8 MiB writes, verify file contents with BLAKE3, validate relative paths, and report progress. Folder structures, empty directories, and basic permission bits are preserved; symbolic links are skipped. Writes are not end-to-end zero-copy because Quinn copies into transport buffers. The four-stream pipeline helps folders with multiple files; a single large file still uses one stream.
- Interrupted file entries can resume from the last complete 8 MiB boundary while both apps remain open and the sender retains the transfer in its current UI session. The receiver stores partial bytes in hidden `.part` files and verifies the completed file before finalizing it. Restart-safe transfer manifests and automatic discovery of old partial transfers are still outstanding; ownership, timestamps, ACLs, and extended attributes are not preserved.
- Pairing QR admission expires after five minutes, but an already-established connection is allowed to finish its transfer.
- Offline networking status: transfers need no internet access when both devices are already on the same reachable LAN. Creating or joining a network for devices with no shared LAN is not implemented. `start_pairing` selects an IPv4 address from existing interfaces and binds QUIC; it does not manage Wi-Fi radios, hotspots, or credentials.
- Native offline-network roadmap:
	- Keep the current QR + QUIC flow as the common transport once a peer-to-peer IP path exists. Treat interface addresses as candidates, since devices may have VPN, cellular, Ethernet, and hotspot interfaces simultaneously; the pairing payload currently carries only one address.
	- Android: investigate `WifiManager.startLocalOnlyHotspot` for a temporary access point and Wi-Fi Direct for peer links. These are distinct paths with different lifecycle, user-consent, and permission requirements. Pass hotspot credentials to the joining peer through an authenticated out-of-band pairing step; never put them in a public discovery record.
	- iOS: investigate `MultipeerConnectivity` for peer discovery and a local data path. iOS does not provide a general app API to create a Wi-Fi hotspot, so do not make hotspot creation a cross-platform assumption. Review required usage descriptions and entitlements for the chosen API.
	- Desktop: investigate NetworkManager D-Bus hotspot control on Linux, Windows Wi-Fi Direct, and the supported Apple networking APIs on macOS. Require explicit user action and handle unavailable radios, OS policy, and permission failures.
	- Implement native adapters behind a small platform-neutral lifecycle contract (availability, start/join, connection details, stop, and errors). Keep QR admission, TLS certificate pinning, bearer-token authentication, and transfer logic independent of the adapter. Add mobile permission/configuration files only after platform projects are generated.
	- Validate each adapter on physical devices: establish a network with internet disabled, pair in both directions, transfer and resume a file, then stop sharing and confirm the network is released. Test cancellation, app suspension, permission denial, and radio/network loss. Until those checks pass, describe the feature as unsupported rather than “offline hotspot sharing.”
- `swarm.rs` builds BLAKE3 manifests and assigns unclaimed chunk indexes. It does not implement peer-to-peer chunk exchange, persistence, or retry policy.
- `auth_bridge.rs` defines a bounded challenge relay interface only. A virtual FIDO2 authenticator requires platform-specific Windows WebAuthn and macOS AuthenticationServices adapters, RP/origin validation, and independent security review. The assertion structs are not an authenticator implementation.
- Clipboard packets received over QUIC are written by the frontend clipboard plugin. There is no background PC clipboard watcher or continuous multi-peer broadcast yet.
- The QR scanner needs a secure WebView context and native camera permission on mobile. Pairing latency and throughput depend on radio conditions, device limits, storage, and OS scheduling. 500–600 MB/s requires a network and devices capable of sustaining that payload rate; the app cannot guarantee it. Wi-Fi generation, channel width, signal quality, concurrent traffic, and disk speed all matter.

The Rust backend was compiled and tested in a disposable Docker container with Tauri's Linux system dependencies. The current test suite includes path validation, resume offset/accounting, folder indexing, token rejection, and a local pinned-certificate QUIC exchange. Real phone-to-PC transfers and throughput still require device testing.