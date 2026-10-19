use std::{net::SocketAddr, sync::Arc};

use anyhow::{bail, Result};
use quinn::{Connection, Endpoint, RecvStream, SendStream};
use rustls::pki_types::{CertificateDer, PrivateKeyDer, PrivatePkcs8KeyDer};
use serde::Serialize;
use tauri::{Emitter, Manager};
use tokio::io::{AsyncReadExt, AsyncWriteExt};

use super::clipboard::{self, ClipboardPacket};

const MAX_CONTROL_FRAME: usize = 1024 * 1024;
const CLIPBOARD_STREAM_KIND: u8 = 0;
const FILE_STREAM_KIND: u8 = 1;

pub struct ServerEndpoint {
    pub endpoint: Endpoint,
    pub certificate_der: Vec<u8>,
}

pub fn bind_server(address: SocketAddr) -> Result<ServerEndpoint> {
    let certificate = rcgen::generate_simple_self_signed(vec!["zeropass.local".to_owned()])?;
    let certificate_der = certificate.cert.der().as_ref().to_vec();
    let private_key = PrivateKeyDer::Pkcs8(PrivatePkcs8KeyDer::from(
        certificate.key_pair.serialize_der(),
    ));
    let mut server_config = quinn::ServerConfig::with_single_cert(
        vec![CertificateDer::from(certificate_der.clone())],
        private_key,
    )?;
    server_config.transport_config(tuned_transport_config());
    let endpoint = Endpoint::server(server_config, address)?;
    Ok(ServerEndpoint {
        endpoint,
        certificate_der,
    })
}

pub(super) fn client_config(certificate_der: &[u8]) -> Result<quinn::ClientConfig> {
    let mut roots = rustls::RootCertStore::empty();
    roots.add(CertificateDer::from(certificate_der.to_vec()))?;
    let mut config = quinn::ClientConfig::with_root_certificates(Arc::new(roots))?;
    config.transport_config(tuned_transport_config());
    Ok(config)
}

fn tuned_transport_config() -> Arc<quinn::TransportConfig> {
    let mut transport = quinn::TransportConfig::default();
    transport.stream_receive_window(quinn::VarInt::from_u32(8 * 1024 * 1024));
    transport.receive_window(quinn::VarInt::from_u32(24 * 1024 * 1024));
    transport.max_concurrent_bidi_streams(quinn::VarInt::from_u32(4));
    Arc::new(transport)
}

pub async fn send_clipboard(
    address: SocketAddr,
    certificate_der: &[u8],
    packet: &ClipboardPacket,
) -> Result<()> {
    let mut endpoint = Endpoint::client("0.0.0.0:0".parse()?)?;
    endpoint.set_default_client_config(client_config(certificate_der)?);
    let connection = endpoint.connect(address, "zeropass.local")?.await?;
    let (mut send, mut receive) = connection.open_bi().await?;
    send.write_u8(CLIPBOARD_STREAM_KIND).await?;
    write_control_frame(&mut send, packet).await?;
    send.finish()?;
    let mut acknowledgement = [0_u8; 2];
    receive.read_exact(&mut acknowledgement).await?;
    connection.close(0_u32.into(), b"done");
    endpoint.wait_idle().await;
    if &acknowledgement != b"OK" {
        bail!("peer rejected clipboard message");
    }
    Ok(())
}

pub async fn probe_peer(address: SocketAddr, certificate_der: &[u8]) -> Result<()> {
    let mut endpoint = Endpoint::client("0.0.0.0:0".parse()?)?;
    endpoint.set_default_client_config(client_config(certificate_der)?);
    let connection = endpoint.connect(address, "zeropass.local")?.await?;
    connection.close(0_u32.into(), b"pair verified");
    endpoint.wait_idle().await;
    Ok(())
}

async fn write_control_frame<T: Serialize>(stream: &mut SendStream, value: &T) -> Result<()> {
    let frame = serde_json::to_vec(value)?;
    if frame.len() > MAX_CONTROL_FRAME {
        bail!("control frame too large");
    }
    stream.write_u32(frame.len() as u32).await?;
    stream.write_all(&frame).await?;
    Ok(())
}

pub async fn serve_connection(connection: Connection, token: String, app: tauri::AppHandle) {
    let transfer_sessions = app
        .state::<super::file_transfer::TransferSessions>()
        .inner()
        .clone();
    while let Ok((mut send, mut receive)) = connection.accept_bi().await {
        let expected_token = token.clone();
        let app_handle = app.clone();
        let sessions = transfer_sessions.clone();
        tokio::spawn(async move {
            let result = async {
                match receive.read_u8().await? {
                    CLIPBOARD_STREAM_KIND => {
                        handle_clipboard_stream(
                            &mut send,
                            &mut receive,
                            &expected_token,
                            &app_handle,
                        )
                        .await
                    }
                    FILE_STREAM_KIND => {
                        super::file_transfer::receive_entry(
                            &mut send,
                            &mut receive,
                            &expected_token,
                            &app_handle,
                            &sessions,
                        )
                        .await
                    }
                    _ => bail!("unknown QUIC stream kind"),
                }
            }
            .await;
            if let Err(error) = result {
                let message = format!("{error:#}");
                eprintln!("ZEROPASS stream failed: {message}");
                let _ = app_handle.emit("file-transfer-error", message);
                let _ = send.reset(1_u32.into());
            }
        });
    }
}

async fn handle_clipboard_stream(
    send: &mut SendStream,
    receive: &mut RecvStream,
    token: &str,
    app: &tauri::AppHandle,
) -> Result<()> {
    let frame_len = receive.read_u32().await? as usize;
    if frame_len == 0 || frame_len > MAX_CONTROL_FRAME {
        bail!("invalid control frame size");
    }
    let mut frame = vec![0; frame_len];
    receive.read_exact(&mut frame).await?;
    let packet: ClipboardPacket = serde_json::from_slice(&frame)?;
    clipboard::validate_packet(&packet, token)?;
    app.emit("clipboard-received", packet.text)?;
    send.write_all(b"OK").await?;
    send.finish()?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use std::{fs, io::Write, time::Instant};

    use anyhow::{ensure, Context, Result};
    use memmap2::MmapOptions;
    use quinn::Endpoint;
    use tokio::io::{AsyncReadExt, AsyncWriteExt};

    use super::{bind_server, client_config};

    #[tokio::test]
    async fn exchanges_data_over_pinned_local_quic() -> Result<()> {
        let server = bind_server("127.0.0.1:0".parse()?)?;
        let address = server.endpoint.local_addr()?;
        let certificate = server.certificate_der.clone();
        let server_endpoint = server.endpoint;
        let server_task = tokio::spawn(async move {
            let incoming = server_endpoint
                .accept()
                .await
                .context("accept local connection")?;
            let connection = incoming.await.context("complete local QUIC handshake")?;
            let (mut send, mut receive) = connection.accept_bi().await?;
            assert_eq!(receive.read_u8().await?, 7);
            send.write_all(b"OK").await?;
            send.finish()?;
            connection.closed().await;
            Ok::<_, anyhow::Error>(())
        });

        let mut endpoint = Endpoint::client("127.0.0.1:0".parse()?)?;
        endpoint.set_default_client_config(client_config(&certificate)?);
        let connection = endpoint.connect(address, "zeropass.local")?.await?;
        let (mut send, mut receive) = connection.open_bi().await?;
        send.write_u8(7).await?;
        send.finish()?;
        let mut response = [0_u8; 2];
        receive.read_exact(&mut response).await?;
        assert_eq!(&response, b"OK");
        connection.close(0_u32.into(), b"test complete");
        endpoint.wait_idle().await;
        server_task
            .await
            .context("local QUIC server task panicked")??;
        Ok(())
    }

    #[tokio::test]
    #[ignore = "manual loopback QUIC and disk throughput benchmark"]
    async fn benchmarks_mmap_file_to_file_quic() -> Result<()> {
        const CHUNK_BYTES: usize = 1024 * 1024;
        let benchmark_mib = std::env::var("ZEROPASS_BENCH_MIB")
            .ok()
            .and_then(|value| value.parse::<u64>().ok())
            .unwrap_or(256);
        let file_size = benchmark_mib.checked_mul(CHUNK_BYTES as u64)
            .context("benchmark size overflow")?;
        let test_dir = std::env::temp_dir().join(format!(
            "zeropass-quic-bench-{}-{}",
            std::process::id(),
            rand::random::<u64>()
        ));
        fs::create_dir_all(&test_dir)?;
        let source_path = test_dir.join("source.bin");
        let destination_path = test_dir.join("destination.bin");

        {
            let mut source = fs::File::create(&source_path)?;
            let pattern = vec![0x5a; CHUNK_BYTES];
            for _ in 0..benchmark_mib {
                source.write_all(&pattern)?;
            }
            source.sync_all()?;
        }

        let server = bind_server("127.0.0.1:0".parse()?)?;
        let address = server.endpoint.local_addr()?;
        let certificate = server.certificate_der.clone();
        let server_endpoint = server.endpoint;
        let receive_path = destination_path.clone();
        let server_task = tokio::spawn(async move {
            let incoming = server_endpoint.accept().await.context("accept benchmark client")?;
            let connection = incoming.await.context("complete benchmark QUIC handshake")?;
            let (_, mut receive) = connection.accept_bi().await?;
            let announced_size = receive.read_u64().await?;
            let mut output = tokio::fs::File::create(receive_path).await?;
            let mut remaining = announced_size;
            let mut buffer = vec![0_u8; CHUNK_BYTES];
            let mut hasher = blake3::Hasher::new();
            while remaining > 0 {
                let amount = remaining.min(buffer.len() as u64) as usize;
                receive.read_exact(&mut buffer[..amount]).await?;
                output.write_all(&buffer[..amount]).await?;
                hasher.update(&buffer[..amount]);
                remaining -= amount as u64;
            }
            let mut expected_hash = [0_u8; 32];
            receive.read_exact(&mut expected_hash).await?;
            output.flush().await?;
            output.sync_all().await?;
            ensure!(expected_hash == *hasher.finalize().as_bytes(), "benchmark file hash mismatch");
            Ok::<u64, anyhow::Error>(announced_size)
        });

        let source = fs::File::open(&source_path)?;
        let mapping = unsafe { MmapOptions::new().map(&source)? };
        let mut hasher = blake3::Hasher::new();
        let mut endpoint = Endpoint::client("127.0.0.1:0".parse()?)?;
        endpoint.set_default_client_config(client_config(&certificate)?);
        let connection = endpoint.connect(address, "zeropass.local")?.await?;
        let (mut send, _) = connection.open_bi().await?;
        let started = Instant::now();
        send.write_u64(file_size).await?;
        for chunk in mapping.chunks(CHUNK_BYTES) {
            hasher.update(chunk);
            send.write_all(chunk).await?;
        }
        send.write_all(hasher.finalize().as_bytes()).await?;
        send.finish()?;
        ensure!(server_task.await.context("benchmark receiver task panicked")?? == file_size,
            "receiver byte count mismatch");
        let elapsed = started.elapsed().as_secs_f64().max(f64::EPSILON);
        connection.close(0_u32.into(), b"benchmark complete");
        endpoint.wait_idle().await;
        eprintln!(
            "QUIC loopback: {benchmark_mib} MiB in {elapsed:.3}s ({:.1} MiB/s)",
            file_size as f64 / elapsed / CHUNK_BYTES as f64
        );

        let source_hash = blake3::hash(&fs::read(&source_path)?);
        let destination_hash = blake3::hash(&fs::read(&destination_path)?);
        assert_eq!(source_hash, destination_hash);
        fs::remove_dir_all(test_dir)?;
        Ok(())
    }
}
