use std::time::Duration;

use color_eyre::{
    Result,
    eyre::{ContextCompat, bail},
};
use futures::{SinkExt, StreamExt};
use tokio::time::timeout;
use tokio_tungstenite::{connect_async, tungstenite::Message};

use crate::twitch::{
    api::messages::{NewTwitchMessage, send_twitch_message},
    context::TwitchWebsocketContext,
};

const TWITCH_IRC_WS_URL: &str = "wss://irc-ws.chat.twitch.tv:443";

/// Handles the user wanting to send a message from the terminal to the WebSocket server
pub async fn handle_send_message(context: &TwitchWebsocketContext, message: String) -> Result<()> {
    let oauth = context
        .oauth()
        .context("Twitch OAuth could not be found when sending message")?;

    if oauth.has_scope("user:write:chat") {
        let twitch_client = context
            .twitch_client()
            .context("Twitch client could not be found when sending message")?;

        let channel_id = context
            .channel_id()
            .context("Channel ID could not be found when sending message")?;

        let user_id = oauth
            .user_id()
            .context("Twitch user ID could not be found when sending message")?;

        let new_message = NewTwitchMessage::new(channel_id.clone(), user_id, message);

        send_twitch_message(&twitch_client, new_message).await?;
        return Ok(());
    }

    if oauth.has_scope("chat:edit") {
        let token = context
            .token()
            .context("Twitch token could not be found for IRC message fallback")?;
        let login = oauth
            .login()
            .context("Twitch login could not be found for IRC message fallback")?;
        let channel_name = context
            .channel_name()
            .context("Channel name could not be found for IRC message fallback")?;

        send_irc_message(token, &login, channel_name, &message).await?;
        return Ok(());
    }

    bail!("OAuth token is missing required scope `user:write:chat` or `chat:edit` to send messages")
}

async fn send_irc_message(
    token: &str,
    login: &str,
    channel_name: &str,
    message: &str,
) -> Result<()> {
    let oauth_pass = if token.starts_with("oauth:") {
        token.to_string()
    } else {
        format!("oauth:{token}")
    };
    let channel = channel_name.trim_start_matches('#').to_lowercase();

    let (mut ws_stream, _) = connect_async(TWITCH_IRC_WS_URL).await?;

    ws_stream
        .send(Message::Text(
            "CAP REQ :twitch.tv/commands twitch.tv/tags".into(),
        ))
        .await?;
    ws_stream
        .send(Message::Text(format!("PASS {oauth_pass}").into()))
        .await?;
    ws_stream
        .send(Message::Text(format!("NICK {login}").into()))
        .await?;
    ws_stream
        .send(Message::Text(
            format!("PRIVMSG #{channel} :{message}").into(),
        ))
        .await?;

    // Briefly read IRC responses to catch authentication orNOTICE rejection messages
    while let Ok(Some(Ok(Message::Text(line)))) =
        timeout(Duration::from_millis(350), ws_stream.next()).await
    {
        let text = line.as_str();
        if text.contains("NOTICE") {
            if let Some((_, notice)) = text.rsplit_once(" :") {
                let notice = notice.trim();
                if !notice.is_empty() {
                    drop(ws_stream.close(None).await);
                    bail!("{notice}");
                }
            }
        }
    }

    drop(ws_stream.close(None).await);
    Ok(())
}
