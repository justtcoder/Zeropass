use anyhow::{bail, Result};
use serde::{Deserialize, Serialize};
use subtle::ConstantTimeEq;

const MAX_CLIPBOARD_BYTES: usize = 1024 * 1024;

#[derive(Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ClipboardPacket {
    pub token: String,
    pub text: String,
}

pub fn validate_packet(packet: &ClipboardPacket, expected_token: &str) -> Result<()> {
    validate_token(&packet.token, expected_token)?;
    if packet.text.len() > MAX_CLIPBOARD_BYTES {
        bail!("clipboard message exceeds the 1 MiB limit");
    }
    Ok(())
}

pub fn validate_token(token: &str, expected_token: &str) -> Result<()> {
    if token.len() != expected_token.len()
        || !bool::from(token.as_bytes().ct_eq(expected_token.as_bytes()))
    {
        bail!("invalid pairing token");
    }
    Ok(())
}
