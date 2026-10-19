use std::time::Duration;

use anyhow::bail;
use serde::{Deserialize, Serialize};
use tokio::sync::{mpsc, oneshot};

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AuthChallenge {
    pub request_id: String,
    pub relying_party_id: String,
    pub challenge: Vec<u8>,
    pub user_presence_required: bool,
    pub user_verification_required: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AuthAssertion {
    pub authenticator_data: Vec<u8>,
    pub client_data_json: Vec<u8>,
    pub signature: Vec<u8>,
    pub credential_id: Vec<u8>,
}

pub struct PendingChallenge {
    pub challenge: AuthChallenge,
    pub reply: oneshot::Sender<AuthAssertion>,
}

pub async fn relay_challenge(
    channel: &mpsc::Sender<PendingChallenge>,
    challenge: AuthChallenge,
) -> anyhow::Result<AuthAssertion> {
    let request_id = challenge.request_id.clone();
    let (reply, result) = oneshot::channel();
    channel.send(PendingChallenge { challenge, reply }).await?;
    match tokio::time::timeout(Duration::from_secs(60), result).await {
        Ok(Ok(assertion)) => Ok(assertion),
        Ok(Err(_)) => bail!("mobile authenticator disconnected for request {request_id}"),
        Err(_) => bail!("mobile authenticator timed out for request {request_id}"),
    }
}

// Platform WebAuthn adapters must validate the origin, challenge, user
// verification, and assertion signature before returning an assertion to the OS.
