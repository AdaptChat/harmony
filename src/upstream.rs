use std::sync::Arc;

use axum::extract::ws::Message;
use deadpool_lapin::Object;
use essence::ws::OutboundMessage;
use futures_util::{future::join_all, TryStreamExt};
use lapin::{
    options::{BasicConsumeOptions, ExchangeDeclareOptions, QueueBindOptions, QueueDeclareOptions},
    types::FieldTable,
    Channel, ExchangeKind,
};
use tokio::sync::{mpsc::UnboundedSender, Notify};

use crate::{
    config::{MessageFormat, UserSession},
    error::Result,
};

async fn subscribe<T: AsRef<str>, U: AsRef<str>, R: AsRef<str>>(
    channel: &Channel,
    guild_id: T,
    session_id: U,
    user_id: R,
) -> Result<()> {
    channel
        .exchange_declare(
            guild_id.as_ref(),
            ExchangeKind::Fanout,
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
            guild_id.as_ref(),
            user_id.as_ref(),
            QueueBindOptions::default(),
            FieldTable::default(),
        )
        .await?;

    Ok(())
}

pub async fn handle_upstream(
    session: UserSession,
    tx: UnboundedSender<Message>,
    amqp: Object,
    finished: Arc<Notify>,
) -> Result<()> {
    let channel = amqp.create_channel().await?;

    channel
        .queue_declare(
            &session.id,
            QueueDeclareOptions {
                auto_delete: true,
                ..Default::default()
            },
            FieldTable::default(),
        )
        .await?;

    let sc = &channel;
    let sid = &session.id;
    let uid = &session.user_id.to_string();

    join_all(
        session
            .get_guilds()
            .await?
            .into_iter()
            .map(async move |guild| -> Result<()> {
                subscribe(sc, guild.partial.id.to_string(), sid, uid).await?;

                Ok(())
            }),
    )
    .await;

    channel
        .queue_bind(
            sid,
            "events",
            &session.user_id.to_string(),
            QueueBindOptions::default(),
            FieldTable::default(),
        )
        .await?;

    let mut consumer = channel
        .basic_consume(
            &session.id,
            &format!("consumer-{}", &session.id),
            BasicConsumeOptions::default(),
            FieldTable::default(),
        )
        .await?;

    let format = session.format.clone();
    let sc = channel.clone();
    let sid = session.id.clone();

    finished.notify_one();

    let main_recv = tokio::spawn(async move || -> Result<()> {
        let ref_sid = &sid;

        while let Ok(Some(m)) = consumer.try_next().await {
            let b = bincode::decode_from_slice::<OutboundMessage, _>(
                &m.data,
                bincode::config::standard(),
            )
            .expect("Server sent unserializable data")
            .0;

            debug!("Got event from upstream: {b:?}");

            tx.send(match format {
                MessageFormat::Json => Message::Text(
                    simd_json::to_string(&b).expect("Server sent unserializable data"),
                ),
                MessageFormat::Msgpack => Message::Binary(
                    rmp_serde::to_vec_named(&b).expect("Server sent unserializable data"),
                ),
            })
            .expect("tx dropped");

            match b {
                OutboundMessage::GuildCreate { guild } => {
                    let id = guild.partial.id.to_string();

                    subscribe(&sc, id, ref_sid, session.user_id.to_string()).await?;
                }
                _ => (),
            }
        }

        Ok(())
    }());

    debug!("Main recv started");

    // There might be another task in the future
    main_recv.await??;

    Ok(())
}
