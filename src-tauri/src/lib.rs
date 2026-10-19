mod engine;

use engine::{
    handshake::{self, PairingPayload},
    quic_engine,
};

#[tauri::command]
async fn start_pairing(app: tauri::AppHandle) -> Result<PairingPayload, String> {
    handshake::start_pairing(app)
        .await
        .map_err(|error| format!("{error:#}"))
}

fn decode_payload(payload: PairingPayload) -> Result<(std::net::SocketAddr, Vec<u8>), String> {
    let address = handshake::parse_address(&payload).map_err(|error| format!("{error:#}"))?;
    let certificate = base64::Engine::decode(
        &base64::engine::general_purpose::STANDARD,
        &payload.certificate_der,
    )
    .map_err(|error| error.to_string())?;
    Ok((address, certificate))
}

#[tauri::command]
async fn connect_peer(payload: PairingPayload) -> Result<(), String> {
    let (address, certificate) = decode_payload(payload)?;
    quic_engine::probe_peer(address, &certificate)
        .await
        .map_err(|error| format!("{error:#}"))
}

#[tauri::command]
async fn send_clipboard(payload: PairingPayload, text: String) -> Result<(), String> {
    let token = payload.token.clone();
    let (address, certificate) = decode_payload(payload)?;
    let packet = engine::clipboard::ClipboardPacket { token, text };
    quic_engine::send_clipboard(address, &certificate, &packet)
        .await
        .map_err(|error| format!("{error:#}"))
}

#[tauri::command]
async fn send_files(
    app: tauri::AppHandle,
    payload: PairingPayload,
    paths: Vec<String>,
    transfer_id: String,
) -> Result<engine::file_transfer::TransferSummary, String> {
    let token = payload.token.clone();
    let (address, certificate) = decode_payload(payload)?;
    engine::file_transfer::send_paths(address, &certificate, &token, transfer_id, paths, app)
        .await
        .map_err(|error| format!("{error:#}"))
}

pub fn run() {
    tauri::Builder::default()
        .manage(engine::file_transfer::new_transfer_sessions())
        .plugin(tauri_plugin_clipboard_manager::init())
        .plugin(tauri_plugin_dialog::init())
        .invoke_handler(tauri::generate_handler![
            start_pairing,
            connect_peer,
            send_clipboard,
            send_files
        ])
        .run(tauri::generate_context!())
        .expect("error while running ZEROPASS");
}

#[cfg(test)]
mod tests {
    use super::engine::clipboard::{validate_packet, ClipboardPacket};

    #[test]
    fn rejects_invalid_pairing_token() {
        let packet = ClipboardPacket {
            token: "wrong".into(),
            text: "hello".into(),
        };
        assert!(validate_packet(&packet, "expected").is_err());
    }
}
