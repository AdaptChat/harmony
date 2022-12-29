use std::{borrow::Cow, net::IpAddr, num::NonZeroU32, time::Duration};

use axum::extract::ws::{CloseFrame, Message, WebSocket};
use essence::ws::{InboundMessage, OutboundMessage};
use futures_util::{
    stream::{SplitSink, StreamExt},
    SinkExt, TryStreamExt,
};
use governor::{Quota, RateLimiter};

use crate::{
    config::{Connection, UserSession},
    error::{Error, Result},
};

async fn handle_error(e: Error, sender: &mut SplitSink<WebSocket, Message>) -> Result<()> {
    match e {
        Error::InvalidData(e) => {
            sender
                .send(Message::Close(Some(CloseFrame {
                    code: 1003,
                    reason: Cow::Owned(e),
                })))
                .await?;

            sender.close().await?;
            Err(Error::Ignore)
        }
        Error::Ignore => Ok(()),
    }
}

/// Process event for a websocket connection.
///
/// # Error
///
/// This function returns an error solely because of the conveience of the ? operator
/// It uses the ? operator on `sender.send` function because if it fails to send that means the client has disconnected, meaning is safe to return
/// The error return by this function is meant to be ignored.
pub async fn handle_socket(socket: WebSocket, con: Connection, ip: IpAddr) -> Result<()> {
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
                Ok(InboundMessage::Identify { token }) => UserSession::new(con, token),
                Err(e) => {
                    handle_error(e, &mut sender).await?;

                    return Ok(());
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

    let ratelimiter =
        unsafe { RateLimiter::direct(Quota::per_minute(NonZeroU32::new_unchecked(1000))) };

    sender
        .send(session.encode(&OutboundMessage::Ready {
            session_id: session.id.clone(),
        }))
        .await?;

    while let Ok(Some(mut message)) = receiver.try_next().await {
        if let Message::Close(_) = &message {
            return Ok(());
        }

        if ratelimiter.check().is_err() {
            info!(
                "Rate limit exceeded for {ip} - {}, disconnecting",
                &session.id
            );

            sender
                .send(Message::Close(Some(CloseFrame {
                    code: 1008,
                    reason: Cow::Borrowed("Rate limit exceeded"),
                })))
                .await?;

            return Ok(());
        }

        let event = session.decode::<InboundMessage>(&mut message);

        match event {
            Ok(event) => match event {
                InboundMessage::Ping => sender.send(session.encode(&OutboundMessage::Pong)).await?,
                _ => {}
            },
            Err(e) => {
                if handle_error(e, &mut sender).await.is_ok() {
                    continue;
                } else {
                    return Ok(());
                }
            }
        }
    }

    Ok(())
}
