use std::{net::IpAddr, time::Duration};

use amqprs::channel::Channel;
use anyhow::{anyhow, Context};
use futures_util::{StreamExt, TryStreamExt};

use crate::{config::UserSession, socket_accept::WebSocketStream};

pub async fn process_events(
    websocket: WebSocketStream,
    amqp: Channel,
    ip: IpAddr,
    session: UserSession,
) -> anyhow::Result<()> {
    let (tx, mut rx) = websocket.split();

    let hello = tokio::time::timeout(Duration::from_secs(5), rx.try_next())
        .await
        .context("timeout")?
        .context("ws error")?
        .ok_or_else(|| anyhow!("stream closed"))?;

    Ok(())
}
