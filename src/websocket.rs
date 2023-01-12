use std::{borrow::Cow, net::IpAddr, num::NonZeroU32, sync::Arc, time::Duration};

use axum::extract::ws::{CloseFrame, Message, WebSocket};
use deadpool_lapin::Object;
use essence::ws::{InboundMessage, OutboundMessage};
use futures_util::{stream::StreamExt, SinkExt, TryStreamExt};
use governor::{Quota, RateLimiter};
use tokio::sync::{mpsc::UnboundedSender, Notify};

use crate::{
    config::{Connection, UserSession},
    error::{Error, Result},
    upstream::handle_upstream,
};

async fn handle_error(e: Error, tx: &UnboundedSender<Message>) -> Result<()> {
    match e {
        Error::Close(e) => {
            tx.send(Message::Close(Some(CloseFrame {
                code: 1003,
                reason: Cow::Owned(e),
            })))?;

            Err(Error::Ignore)
        }
        Error::Ignore => Ok(()),
    }
}

macro_rules! close_if_error {
    ($func:expr, $sender:expr) => {{
        match $func {
            Ok(val) => val,
            Err(e) => return handle_error(e, $sender).await,
        }
    }};
}

/// Process event for a websocket connection.
///
/// # Error
///
/// This function returns an error solely because of the conveience of the ? operator
/// It uses the ? operator on `sender.send` function because if it fails to send that means the client has disconnected, meaning is safe to return
/// The error return by this function is meant to be ignored.
pub async fn handle_socket(
    socket: WebSocket,
    con: Connection,
    ip: IpAddr,
    amqp: Object,
) -> Result<()> {
    debug!("Connected from: {ip}");

    let (mut sender, mut receiver) = socket.split();
    sender.send(con.encode(&OutboundMessage::Hello)).await?;

    let session = {
        if let Ok(Ok(Some(mut message))) =
            tokio::time::timeout(Duration::from_secs(10), receiver.try_next()).await
        {
            if let Message::Close(_) = &message {
                return Ok(());
            }

            let event = con.decode::<InboundMessage>(&mut message);

            match event {
                Ok(InboundMessage::Identify { token }) => {
                    match UserSession::new_with_token(con, token).await {
                        Ok(val) => val,
                        Err(e) => {
                            return Ok(sender
                                .send(Message::Close(Some(CloseFrame {
                                    code: 1003,
                                    reason: Cow::Owned(e.to_string()),
                                })))
                                .await?);
                        }
                    }
                }
                Err(e) => {
                    return Ok(sender
                        .send(Message::Close(Some(CloseFrame {
                            code: 1003,
                            reason: Cow::Owned(e.to_string()),
                        })))
                        .await?)
                }
                _ => {
                    sender
                        .send(Message::Close(Some(CloseFrame {
                            code: 1003,
                            reason: Cow::Borrowed("Failed to send Identify event"),
                        })))
                        .await?;
                    sender.close().await?;

                    return Ok(());
                }
            }
        } else {
            sender
                .send(Message::Close(Some(CloseFrame {
                    code: 1003,
                    reason: Cow::Borrowed("Failed to send Identify event"),
                })))
                .await?;

            sender.close().await?;

            return Ok(());
        }
    };

    let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel::<Message>();

    let rx_task = tokio::spawn(async move {
        while let Some(m) = rx.recv().await {
            sender.send(m).await.unwrap();
        }
    });

    let upstream_finished_setup = Arc::new(Notify::new());

    let upstream_task = tokio::spawn(handle_upstream(
        session.clone(),
        tx.clone(),
        amqp,
        upstream_finished_setup.clone(),
    ));

    let ratelimiter =
        unsafe { RateLimiter::direct(Quota::per_minute(NonZeroU32::new_unchecked(1000))) };

    upstream_finished_setup.notified().await;
    tx.send(session.encode(&close_if_error!(session.get_ready_event().await, &tx)))?;

    let client_task = tokio::spawn(async move || -> Result<()> {
        while let Ok(Some(mut message)) = receiver.try_next().await {
            if let Message::Close(_) = &message {
                return Ok(());
            }

            if ratelimiter.check().is_err() {
                info!(
                    "Rate limit exceeded for {ip} - {}, disconnecting",
                    &session.id
                );

                tx.send(Message::Close(Some(CloseFrame {
                    code: 1008,
                    reason: Cow::Borrowed("Rate limit exceeded"),
                })))?;

                return Ok(());
            }

            let event = session.decode::<InboundMessage>(&mut message);

            match event {
                Ok(event) => match event {
                    InboundMessage::Ping => tx.send(session.encode(&OutboundMessage::Pong))?,
                    _ => {}
                },
                Err(e) => {
                    if handle_error(e, &tx).await.is_ok() {
                        continue;
                    } else {
                        return Ok(());
                    }
                }
            }
        }

        Ok(())
    }());

    match tokio::try_join!(rx_task, upstream_task, client_task)? {
        (_, upstream, client) => {
            upstream?;
            client?;
        }
    }

    Ok(())
}
