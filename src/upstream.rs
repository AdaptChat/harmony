use std::sync::Arc;

use axum::extract::ws::Message;
use deadpool_lapin::Object;
use flume::Sender;
use futures_util::future::join_all;
use lapin::{
    options::{BasicConsumeOptions, ExchangeDeclareOptions, QueueBindOptions, QueueDeclareOptions},
    types::FieldTable,
    Channel, ExchangeKind,
};
use tokio::sync::{Notify, RwLock};

use crate::{config::UserSession, error::Result, recv};

pub async fn subscribe(
    channel: &Channel,
    guild_id: impl AsRef<str>,
    session_id: impl AsRef<str>,
    user_id: impl AsRef<str>,
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
    tx: Sender<Message>,
    amqp: Object,
    finished: Arc<Notify>,
) -> Result<()> {
    let channel = amqp.create_channel().await?;
    let user_id = session.user_id.to_string();
    let session_id = session.id.clone();

    channel
        .queue_declare(
            &session_id,
            QueueDeclareOptions {
                auto_delete: true,
                ..Default::default()
            },
            FieldTable::default(),
        )
        .await?;

    {
        let sc = &channel;
        let ref_user_id = &user_id;
        let ref_session_id = &session_id;

        join_all(
            session
                .get_guilds()
                .await?
                .into_iter()
                .map(async move |guild| -> Result<()> {
                    subscribe(
                        sc,
                        guild.partial.id.to_string(),
                        ref_session_id,
                        ref_user_id,
                    )
                    .await?;

                    Ok(())
                }),
        )
        .await;
    }

    channel
        .queue_bind(
            &session_id,
            "events",
            &user_id,
            QueueBindOptions::default(),
            FieldTable::default(),
        )
        .await?;

    let consumer = channel
        .basic_consume(
            &session_id,
            &format!("consumer-{}-{}", &user_id, &session.id),
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
        session.format,
        session_id,
        user_id,
        Arc::new(RwLock::new(session)),
    )
    .await?;

    Ok(())
}
