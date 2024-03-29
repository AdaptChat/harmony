use std::{net::IpAddr, sync::Arc};

use deadpool_lapin::Object;
use essence::{
    db::{get_pool, ChannelDbExt},
    models::Guild,
};
use flume::Sender;
use futures_util::future::{JoinAll};
use lapin::{
    options::{BasicConsumeOptions, ExchangeDeclareOptions, QueueBindOptions, QueueDeclareOptions},
    types::FieldTable,
    Channel, ExchangeKind,
};
use tokio::sync::Notify;
use tokio_tungstenite::tungstenite::Message;

use crate::{
    config::{HiddenChannels, MessageFormat},
    error::{Result},
    recv,
};

pub async fn subscribe(
    channel: &Channel,
    exchange: impl AsRef<str>,
    session_id: impl AsRef<str>,
    kind: ExchangeKind,
) -> Result<()> {
    channel
        .exchange_declare(
            exchange.as_ref(),
            kind,
            ExchangeDeclareOptions {
                auto_delete: true,
                ..Default::default()
            },
            FieldTable::default(),
        )
        .await?;

    // Routing key will be used to determine intent when implemented
    channel
        .queue_bind(
            session_id.as_ref(),
            exchange.as_ref(),
            "all",
            QueueBindOptions::default(),
            FieldTable::default(),
        )
        .await?;

    Ok(())
}

pub async fn handle_upstream(
    user_id: impl AsRef<str>,
    session_id: impl AsRef<str>,
    tx: Sender<Message>,
    amqp: Object,
    finished: Arc<Notify>,
    ip: IpAddr,
    hidden_channels: HiddenChannels,
    guilds: Vec<Guild>,
    message_format: MessageFormat,
) -> Result<()> {
    let channel = amqp.create_channel().await?;
    let session_id = session_id.as_ref();
    let user_id = user_id.as_ref();

    channel
        .queue_declare(
            session_id.as_ref(),
            QueueDeclareOptions {
                auto_delete: true,
                ..Default::default()
            },
            FieldTable::default(),
        )
        .await?;

    let ref_channel = &channel;

    for res in guilds
        .into_iter()
        .map(|guild| {
            subscribe(
                ref_channel,
                guild.partial.id.to_string(),
                session_id,
                ExchangeKind::Topic,
            )
        })
        .collect::<JoinAll<_>>()
        .await
    {
        res?
    }

    for res in get_pool()
        .fetch_all_dm_channels_for_user(user_id.parse().expect("Not valid user id"))
        .await?
        .into_iter()
        .map(|g| {
            subscribe(
                ref_channel,
                g.id.to_string(),
                session_id,
                ExchangeKind::Fanout,
            )
        })
        .collect::<JoinAll<_>>()
        .await
    {
        res?
    }

    channel
        .queue_bind(
            session_id.as_ref(),
            "events",
            user_id.as_ref(),
            QueueBindOptions::default(),
            FieldTable::default(),
        )
        .await?;

    let consumer = channel
        .basic_consume(
            session_id.as_ref(),
            &format!("consumer-{}-{}-{}", user_id, session_id, ip),
            BasicConsumeOptions::default(),
            FieldTable::default(),
        )
        .await?;

    finished.notify_one();
    debug!("Starting main recv");

    recv::process(
        consumer,
        channel,
        tx,
        message_format,
        session_id,
        Arc::new(hidden_channels),
    )
    .await?;

    Ok(())
}
