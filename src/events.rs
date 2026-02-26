use crate::error::Result;
use ahash::{HashSet, HashSetExt};
use amqprs::{
    channel::{
        BasicPublishArguments, Channel, ExchangeDeclareArguments, ExchangeType, QueueBindArguments,
        QueueUnbindArguments,
    },
    BasicProperties,
};
use bincode::{config::Configuration, Encode};
use std::sync::Mutex;
use tokio::sync::OnceCell;

static DECLARED_EVENTS_EXCHANGE: OnceCell<()> = OnceCell::const_new();
static DECLARED_GUILD_EXCHANGES: Mutex<Option<HashSet<u64>>> = Mutex::new(None);

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

async fn ensure_events_exchange_declared(channel: &Channel) -> Result<()> {
    DECLARED_EVENTS_EXCHANGE
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

    Ok(())
}

fn mark_guild_exchange_declared(exchange_id: u64) -> bool {
    let mut guard = DECLARED_GUILD_EXCHANGES.lock().unwrap();
    let set = guard.get_or_insert_with(HashSet::new);
    !set.insert(exchange_id)
}

fn unmark_guild_exchange_declared(exchange_id: u64) {
    let mut guard = DECLARED_GUILD_EXCHANGES.lock().unwrap();
    if let Some(set) = guard.as_mut() {
        set.remove(&exchange_id);
    }
}

async fn ensure_guild_exchange_declared(channel: &Channel, guild_id: u64) -> Result<()> {
    if !mark_guild_exchange_declared(guild_id) {
        if let Err(e) = channel
            .exchange_declare(
                ExchangeDeclareArguments::of_type(&guild_id.to_string(), ExchangeType::Topic)
                    .auto_delete(false)
                    .finish(),
            )
            .await
        {
            unmark_guild_exchange_declared(guild_id);
            return Err(e.into());
        }
        debug!("declared guild exchange {}", guild_id);
    }
    Ok(())
}

async fn publish_global(channel: &Channel, routing_key: &str, bytes: Vec<u8>) -> Result<()> {
    ensure_events_exchange_declared(channel).await?;
    channel
        .basic_publish(
            BasicProperties::default(),
            bytes,
            BasicPublishArguments::new("events", routing_key),
        )
        .await?;

    Ok(())
}

async fn publish_guild(
    channel: &Channel,
    guild_id: u64,
    routing_key: &str,
    bytes: Vec<u8>,
) -> Result<()> {
    ensure_guild_exchange_declared(channel, guild_id).await?;
    channel
        .basic_publish(
            BasicProperties::default(),
            bytes,
            BasicPublishArguments::new(&guild_id.to_string(), routing_key),
        )
        .await?;
    debug!(
        "published message to guild exchange {} for routing key {}",
        guild_id, routing_key
    );

    Ok(())
}

pub async fn publish_user_event(channel: &Channel, user_id: u64, bytes: Vec<u8>) -> Result<()> {
    publish_global(channel, &user_id.to_string(), bytes).await
}

pub async fn publish_bulk_event(
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

    publish_global(channel, &routing_key, encode(event)?).await
}

pub async fn publish_guild_event(
    channel: &Channel,
    guild_id: u64,
    event: impl Encode,
) -> Result<()> {
    // routing_key all will be replaced with intent.
    publish_guild(channel, guild_id, "all", encode(event)?).await
}

pub async fn subscribe(
    channel: &Channel,
    exchange_id: u64,
    session_id: impl ToString,
    kind: impl ToString,
) -> Result<()> {
    let exchange = exchange_id.to_string();
    let session_id = session_id.to_string();

    if !mark_guild_exchange_declared(exchange_id) {
        if let Err(e) = channel
            .exchange_declare(ExchangeDeclareArguments {
                exchange: exchange.clone(),
                exchange_type: kind.to_string(),
                auto_delete: false,
                no_wait: true,
                ..Default::default()
            })
            .await
        {
            unmark_guild_exchange_declared(exchange_id);
            return Err(e.into());
        }
    }

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
