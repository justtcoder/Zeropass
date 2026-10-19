use std::{
    net::{IpAddr, Ipv4Addr, SocketAddr},
    sync::{
        atomic::{AtomicUsize, Ordering},
        Arc,
    },
    time::Duration,
};

use anyhow::{Context, Result};
use rand::{rngs::OsRng, RngCore};
use serde::{Deserialize, Serialize};
use tauri::Manager;

use super::quic_engine;

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PairingPayload {
    pub ip: String,
    pub port: u16,
    pub certificate_der: String,
    pub token: String,
    pub expires_in_seconds: u64,
}

pub async fn start_pairing(app: tauri::AppHandle) -> Result<PairingPayload> {
    let ip = local_ipv4()?;
    let server = quic_engine::bind_server(SocketAddr::new(IpAddr::V4(Ipv4Addr::UNSPECIFIED), 0))?;
    let port = server.endpoint.local_addr()?.port();
    let certificate_der = server.certificate_der;
    let mut token_bytes = [0_u8; 32];
    OsRng.fill_bytes(&mut token_bytes);
    let token = hex::encode(token_bytes);
    let task_token = token.clone();

    tokio::spawn(async move {
        let endpoint = server.endpoint;
        let transfer_sessions = app
            .state::<super::file_transfer::TransferSessions>()
            .inner()
            .clone();
        let active_connections = Arc::new(AtomicUsize::new(0));
        let drained = Arc::new(tokio::sync::Notify::new());
        let mut pairing_open = true;
        let expires = tokio::time::sleep(Duration::from_secs(300));
        tokio::pin!(expires);
        loop {
            tokio::select! {
                incoming = endpoint.accept() => match incoming {
                    Some(incoming) if pairing_open
                        || super::file_transfer::has_incomplete_transfers(&transfer_sessions).await => {
                        let peer_token = task_token.clone();
                        let app_handle = app.clone();
                        let active = active_connections.clone();
                        let notify = drained.clone();
                        active.fetch_add(1, Ordering::Relaxed);
                        tokio::spawn(async move {
                            if let Ok(connection) = incoming.await {
                                quic_engine::serve_connection(connection, peer_token, app_handle).await;
                            }
                            if active.fetch_sub(1, Ordering::AcqRel) == 1 {
                                notify.notify_one();
                            }
                        });
                    }
                    Some(incoming) => incoming.refuse(),
                    None => break,
                },
                _ = &mut expires, if pairing_open => {
                    pairing_open = false;
                    if active_connections.load(Ordering::Acquire) == 0 {
                        break;
                    }
                }
                _ = drained.notified(), if !pairing_open => {
                    if active_connections.load(Ordering::Acquire) == 0
                        && !super::file_transfer::has_incomplete_transfers(&transfer_sessions).await {
                        break;
                    }
                }
            }
        }
    });

    Ok(PairingPayload {
        ip: ip.to_string(),
        port,
        certificate_der: base64::Engine::encode(
            &base64::engine::general_purpose::STANDARD,
            certificate_der,
        ),
        token,
        expires_in_seconds: 300,
    })
}

fn local_ipv4() -> Result<Ipv4Addr> {
    let interfaces = if_addrs::get_if_addrs().context("enumerate network interfaces")?;
    let addresses: Vec<Ipv4Addr> = interfaces
        .into_iter()
        .filter_map(|interface| match interface.ip() {
            IpAddr::V4(address) if !address.is_loopback() && !address.is_unspecified() => {
                Some(address)
            }
            _ => None,
        })
        .collect();
    addresses
        .iter()
        .copied()
        .find(Ipv4Addr::is_private)
        .or_else(|| addresses.into_iter().next())
        .context("no usable IPv4 address found")
}

pub fn parse_address(payload: &PairingPayload) -> Result<SocketAddr> {
    if payload.expires_in_seconds == 0 || payload.token.len() != 64 {
        anyhow::bail!("invalid or expired pairing payload");
    }
    Ok(SocketAddr::new(payload.ip.parse()?, payload.port))
}
