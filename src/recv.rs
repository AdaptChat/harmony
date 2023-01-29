use std::sync::Arc;

use essence::ws::OutboundMessage;
use flume::Sender;
use futures_util::TryStreamExt;
use lapin::{message::Delivery, options::BasicAckOptions, types::FieldTable, Channel, Consumer};
use tokio_tungstenite::tungstenite::Message;

use crate::{
    config::{HiddenChannels, MessageFormat},
    error::{Error, NackExt, Result},
    upstream,
};

enum HandleState {
    Continue,
    Break,
}

async fn handle(
    m: Delivery,
    channel: Arc<Channel>,
    tx: Sender<Message>,
    message_format: MessageFormat,
    session_id: String,
    user_id: String,
    hidden_channels: Arc<HiddenChannels>,
) -> Result<()> {
    let b_res =
        bincode::decode_from_slice::<OutboundMessage, _>(&m.data, bincode::config::standard());
    let acker = m.acker;

    let b = b_res
        .unwrap_or_nack(&acker, "Server sent unserializable data")
        .await
        .0;

    debug!("Got event from upstream: {b:?}");

    let r = match &b {
        OutboundMessage::GuildCreate { guild } => {
            let id = guild.partial.id.to_string();

            upstream::subscribe(&channel, id, &session_id, &user_id)
                .await
                .map(|_| HandleState::Continue)
        }
        OutboundMessage::GuildRemove { guild_id, .. } => channel
            .queue_unbind(
                &session_id,
                &guild_id.to_string(),
                &user_id,
                FieldTable::default(),
            )
            .await
            .map(|_| HandleState::Continue)
            .map_err(|e| e.into()),
        OutboundMessage::MessageCreate { message } => {
            if hidden_channels.contains(&message.channel_id) {
                Ok(HandleState::Break)
            } else {
                Ok(HandleState::Continue)
            }
        }
        // FIXME: MessageDelete
        OutboundMessage::MessageUpdate { after, .. } => {
            if hidden_channels.contains(&after.channel_id) {
                Ok(HandleState::Break)
            } else {
                Ok(HandleState::Continue)
            }
        }
        _ => Ok::<HandleState, Error>(HandleState::Continue),
    }
    .unwrap_or_nack(&acker, "error while processing incoming event")
    .await;

    match r {
        HandleState::Break => return Ok(()),
        HandleState::Continue => (),
    }

    tx.send(match message_format {
        MessageFormat::Json => Message::Text(
            simd_json::to_string(&b)
                .unwrap_or_nack(&acker, "Server sent unserializable data")
                .await,
        ),
        MessageFormat::Msgpack => Message::Binary(
            rmp_serde::to_vec_named(&b)
                .unwrap_or_nack(&acker, "Server sent unserializable data")
                .await,
        ),
    })
    .unwrap_or_nack(&acker, "tx dropped")
    .await;

    acker.ack(BasicAckOptions::default()).await?;

    Ok(())
}

pub async fn process(
    mut consumer: Consumer,
    channel: Channel,
    tx: Sender<Message>,
    message_format: MessageFormat,
    session_id: impl AsRef<str>,
    user_id: impl AsRef<str>,
    hidden_channels: Arc<HiddenChannels>,
) -> Result<()> {
    // Channel uses 8 Arcs internally, so instead of cloning
    // 8 Arcs everytime there is an event, cloning one is more efficent
    let channel = Arc::new(channel);

    while let Ok(Some(m)) = consumer.try_next().await {
        tokio::spawn(handle(
            m,
            channel.clone(),
            tx.clone(),
            message_format,
            session_id.as_ref().to_string(),
            user_id.as_ref().to_string(),
            hidden_channels.clone(),
        ));
    }

    Ok(())
}
