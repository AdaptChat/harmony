use std::sync::Arc;

use axum::extract::ws::Message;
use deadpool_lapin::Object;
use essence::ws::OutboundMessage;
use futures_util::{future::join_all, TryStreamExt};
use lapin::{
    options::{BasicConsumeOptions, ExchangeDeclareOptions, QueueBindOptions, QueueDeclareOptions},
    types::FieldTable,
    ExchangeKind,
};
use tokio::sync::{mpsc::UnboundedSender, Notify};

use crate::{
    config::{MessageFormat, UserSession},
    error::Result,
};

pub async fn handle_upsteam(
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

    let c = &channel;
    let sid = &session.id;

    join_all(
        session
            .get_guilds()
            .await?
            .into_iter()
            .map(async move |guild| -> Result<()> {
                let id = guild.partial.id.to_string();

                c.exchange_declare(
                    &id,
                    ExchangeKind::Fanout,
                    ExchangeDeclareOptions {
                        auto_delete: true,
                        ..Default::default()
                    },
                    FieldTable::default(),
                )
                .await?;

                // Routing key will be used to determine intent when implemented
                c.queue_bind(
                    sid,
                    &id,
                    sid,
                    QueueBindOptions::default(),
                    FieldTable::default(),
                )
                .await?;

                Ok(())
            }),
    )
    .await;

    finished.notify_one();

    let mut consumer = channel
        .basic_consume(
            &session.id,
            &format!("consumer-{}", &session.id),
            BasicConsumeOptions::default(),
            FieldTable::default(),
        )
        .await?;

    while let Ok(Some(m)) = consumer.try_next().await {
        tx.send(match session.format {
            MessageFormat::Json => Message::Text(
                simd_json::to_string(
                    &rmp_serde::from_slice::<OutboundMessage>(&m.data)
                        .expect("Server sent unserializable data"),
                )
                .expect("Server sent unserializable data"),
            ),
            MessageFormat::Msgpack => Message::Binary(m.data),
        })?;
    }
    Ok(())
}
