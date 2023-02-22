use std::sync::OnceLock;

use bincode::Encode;
use deadpool_lapin::Object;
use lapin::{options::{BasicPublishOptions, ExchangeDeclareOptions}, BasicProperties, ExchangeKind, types::FieldTable};

use crate::error::Result;

static CONNECTION: OnceLock<Object> = OnceLock::new();

pub fn setup(con: Object) {
    CONNECTION.set(con).expect("Connection already set");
}

fn get_con() -> &'static Object {
    CONNECTION.get().expect("Connection not set")
}

async fn publish(
    exchange: impl AsRef<str>,
    routing_key: impl AsRef<str>,
    data: impl Encode,
) -> Result<()> {
    let channel = get_con()
        .create_channel()
        .await?;

    channel
        .exchange_declare(
            exchange.as_ref(),
            ExchangeKind::Fanout,
            ExchangeDeclareOptions {
                auto_delete: true,
                ..Default::default()
            },
            FieldTable::default(),
        )
        .await?;

    channel
        .basic_publish(
            exchange.as_ref(),
            routing_key.as_ref(),
            BasicPublishOptions::default(),
            &bincode::encode_to_vec(data, bincode::config::standard())?,
            BasicProperties::default(),
        )
        .await?;

    Ok(())
}

pub async fn publish_user_event(user_id: u64, event: impl Encode) -> Result<()> {
    publish("events", user_id.to_string(), event).await?;

    Ok(())
}

pub async fn publish_guild_event(guild_id: u64, event: impl Encode) -> Result<()> {
    publish(guild_id.to_string(), "*", event).await?;

    Ok(())
}
