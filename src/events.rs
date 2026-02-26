use crate::error::Result;
use amqprs::{
    channel::{
        BasicPublishArguments, Channel, ExchangeDeclareArguments, ExchangeType, QueueBindArguments,
        QueueUnbindArguments,
    },
    BasicProperties,
};
use bincode::{config::Configuration, Encode};
use tokio::sync::OnceCell;

static EVENTS_EXCHANGE: OnceCell<()> = OnceCell::const_new();

// static CHANNEL: OnceLock<Channel> = OnceLock::new();
pub const CONFIG: Configuration = bincode::config::standard();

// pub fn setup(channel: Channel) {
//     let _ = CHANNEL.set(channel);
// }

// fn get_channel() -> &'static Channel {
//     CHANNEL.get().expect("channel not set")
// }

pub fn encode<T: Encode>(data: T) -> Result<Vec<u8>> {
    bincode::encode_to_vec(data, CONFIG).map_err(Into::into)
}

async fn publish(
    channel: &Channel,
    exchange: &str,
    exchange_auto_delete: bool,
    routing_key: &str,
    bytes: Vec<u8>,
) -> Result<()> {
    // let channel = get_channel();

    channel
        .exchange_declare(
            ExchangeDeclareArguments::of_type(exchange, ExchangeType::Topic)
                .auto_delete(exchange_auto_delete)
                .finish(),
        )
        .await?;
    debug!("declared exchange {}", exchange);

    channel
        .basic_publish(
            BasicProperties::default(),
            bytes,
            BasicPublishArguments::new(exchange, routing_key),
        )
        .await?;
    debug!(
        "published message to exchange {} for routing key {}",
        exchange, routing_key
    );

    Ok(())
}

async fn publish_event(channel: &Channel, routing_key: &str, bytes: Vec<u8>) -> Result<()> {
    EVENTS_EXCHANGE
        .get_or_try_init(|| async {
            channel
                .exchange_declare(
                    ExchangeDeclareArguments::of_type("events", ExchangeType::Topic)
                        .auto_delete(false)
                        .finish(),
                )
                .await?;
            Ok::<(), crate::error::Error>(())
        })
        .await?;

    channel
        .basic_publish(
            BasicProperties::default(),
            bytes,
            BasicPublishArguments::new("events", routing_key),
        )
        .await?;

    Ok(())
}

pub async fn publish_user_event(channel: &Channel, user_id: u64, bytes: Vec<u8>) -> Result<()> {
    publish_event(channel, &user_id.to_string(), bytes).await
}

pub async fn _publish_bulk_event(
    channel: &Channel,
    user_ids: impl AsRef<[u64]>,
    event: impl Encode,
) -> Result<()> {
    let routing_key = user_ids
        .as_ref()
        .iter()
        .map(ToString::to_string)
        .collect::<Vec<_>>()
        .join(".");

    publish_event(channel, &routing_key, encode(event)?).await
}

pub async fn _publish_guild_event(
    channel: &Channel,
    guild_id: u64,
    event: impl Encode,
) -> Result<()> {
    // routing_key all will be replaced with intent.
    publish(channel, &guild_id.to_string(), true, "all", encode(event)?).await
}

pub async fn subscribe(
    channel: &Channel,
    exchange: impl ToString,
    session_id: impl ToString,
    kind: impl ToString,
) -> Result<()> {
    let exchange = exchange.to_string();
    let session_id = session_id.to_string();

    channel
        .exchange_declare(ExchangeDeclareArguments {
            exchange: exchange.clone(),
            exchange_type: kind.to_string(),
            auto_delete: true,
            no_wait: true,
            ..Default::default()
        })
        .await?;

    channel
        .queue_bind(QueueBindArguments {
            queue: session_id,
            exchange,
            routing_key: "all".to_string(), // to be replaced by intents
            no_wait: true,
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
