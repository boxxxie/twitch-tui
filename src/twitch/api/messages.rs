use color_eyre::{
    Result,
    eyre::{ContextCompat, bail},
};
use reqwest::Client;
use serde::{Deserialize, Serialize};

use super::TWITCH_API_BASE_URL;

#[derive(Deserialize, Serialize, Debug, Clone)]
pub struct NewTwitchMessage {
    #[serde(rename = "broadcaster_id")]
    channel_id: String,
    #[serde(rename = "sender_id")]
    user_id: String,
    message: String,
}

impl NewTwitchMessage {
    pub const fn new(broadcaster_id: String, user_id: String, message: String) -> Self {
        Self {
            channel_id: broadcaster_id,
            user_id,
            message,
        }
    }
}

#[derive(Deserialize, Serialize, Debug, Clone)]
pub struct TwitchMessageDropReason {
    pub code: String,
    pub message: String,
}

#[derive(Deserialize, Serialize, Debug, Clone)]
pub struct TwitchNewMessageResponse {
    pub message_id: String,
    pub is_sent: bool,
    pub drop_reason: Option<TwitchMessageDropReason>,
}

#[derive(Deserialize, Serialize, Debug, Clone)]
struct TwitchNewMessageResponseList {
    data: Vec<TwitchNewMessageResponse>,
}

/// Sends a message to the respective channel.
///
/// <https://dev.twitch.tv/docs/api/reference/#send-chat-message>
pub async fn send_twitch_message(
    client: &Client,
    new_message: NewTwitchMessage,
) -> Result<TwitchNewMessageResponse> {
    let url = format!("{TWITCH_API_BASE_URL}/chat/messages");

    let response = client.post(url).json(&new_message).send().await?;
    if !response.status().is_success() {
        let status = response.status();
        let body = response.text().await.unwrap_or_default();
        bail!("Twitch API error ({status}): {body}");
    }

    let response_data = response
        .json::<TwitchNewMessageResponseList>()
        .await?
        .data
        .first()
        .context("Could not get new Twitch message response")?
        .clone();

    if !response_data.is_sent {
        let reason = response_data.drop_reason.as_ref().map_or_else(
            || "Message was dropped by Twitch".to_string(),
            |r| format!("{} ({})", r.message, r.code),
        );
        bail!("{reason}");
    }

    Ok(response_data)
}
