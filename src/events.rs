use std::sync::OnceLock;

use crate::error::Result;
use amqprs::{
    channel::{
        BasicPublishArguments, Channel, ExchangeDeclareArguments, ExchangeType, QueueBindArguments,
        QueueUnbindArguments,
    },
    BasicProperties,
};
use bincode::{config::Configuration, Encode};

// static CHANNEL: OnceLock<Channel> = OnceLock::new();
pub const CONFIG: Configuration = bincode::config::standard();

// pub fn setup(channel: Channel) {
//     let _ = CHANNEL.set(channel);
// }

// fn get_channel() -> &'static Channel {
//     CHANNEL.get().expect("channel not set")
// }

async fn publish(
    channel: &Channel,
    exchange: impl ToString,
    exchange_auto_delete: bool,
    routing_key: impl ToString,
    data: impl Encode,
) -> Result<()> {
    // let channel = get_channel();

    channel
        .exchange_declare(
            ExchangeDeclareArguments::of_type(&exchange.to_string(), ExchangeType::Topic)
                .auto_delete(exchange_auto_delete)
                .finish(),
        )
        .await?;
    debug!("declared exchange {}", exchange.to_string());

    channel
        .basic_publish(
            BasicProperties::default(),
            bincode::encode_to_vec(data, CONFIG)?,
            BasicPublishArguments::new(&exchange.to_string(), &routing_key.to_string()),
        )
        .await?;
    debug!(
        "published message to exchange {} for routing key {}",
        exchange.to_string(),
        routing_key.to_string()
    );

    Ok(())
}

pub async fn publish_user_event(
    channel: &Channel,
    user_id: u64,
    event: impl Encode,
) -> Result<()> {
    publish(channel, "events", false, user_id.to_string(), event).await?;

    Ok(())
}

pub async fn publish_bulk_event(
    channel: &Channel,
    user_ids: impl AsRef<[u64]>,
    event: impl Encode,
) -> Result<()> {
    let routing_key = user_ids
        .as_ref()
        .into_iter()
        .map(ToString::to_string)
        .collect::<Vec<_>>()
        .join(".");

    publish(channel, "events", false, routing_key, event).await?;

    Ok(())
}

pub async fn _publish_guild_event(
    channel: &Channel,
    guild_id: u64,
    event: impl Encode,
) -> Result<()> {
    publish(channel, guild_id.to_string(), true, "all", event).await?; // routing_key all will be replaced with intent.

    Ok(())
}

pub async fn subscribe(
    channel: &Channel,
    exchange: impl ToString,
    session_id: impl ToString,
    kind: impl ToString,
) -> Result<()> {
    channel
        .exchange_declare(ExchangeDeclareArguments {
            exchange: exchange.to_string(),
            exchange_type: kind.to_string(),
            auto_delete: true,
            ..Default::default()
        })
        .await?;

    channel
        .queue_bind(QueueBindArguments {
            queue: session_id.to_string(),
            exchange: exchange.to_string(),
            routing_key: "all".to_string(), // to be replaced by intents
            ..Default::default()
        })
        .await?;

    Ok(())
}

pub async fn unsubscribe(
    channel: &Channel,
    exchange: impl ToString,
    session_id: impl ToString,
) -> Result<()> {
    channel
        .queue_unbind(QueueUnbindArguments {
            queue: session_id.to_string(),
            exchange: exchange.to_string(),
            routing_key: "all".to_string(),
            ..Default::default()
        })
        .await?;

    Ok(())
}
