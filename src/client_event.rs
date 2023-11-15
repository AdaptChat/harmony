use std::net::IpAddr;

use amqprs::channel::{Channel, QueueDeclareArguments};
use anyhow::Context;

use crate::{config::UserSession, socket::WebSocketStream};

pub async fn handle(
    websocket: WebSocketStream,
    amqp_channel: Channel,
    ip: IpAddr,
    session: UserSession,
) -> anyhow::Result<()> {
    amqp_channel
        .queue_declare(
            QueueDeclareArguments::new(session.get_session_id_str())
                .auto_delete(true)
                .finish(),
        )
        .await
        .context("failed to declare queue")?;

    Ok(())
}
