use std::{borrow::Cow, net::IpAddr, time::Duration};

use axum::extract::ws::{CloseFrame, Message, WebSocket};
use essence::ws::{InboundMessage, OutboundMessage};
use futures_util::{
    stream::{SplitSink, StreamExt},
    SinkExt, TryStreamExt,
};

use crate::{
    config::{Connection, UserSession},
    error::{Error, Result},
};

async fn handle_error(e: Error, sender: &mut SplitSink<WebSocket, Message>) -> Option<()> {
    match e {
        Error::InvalidData => {
            drop(
                sender
                    .send(Message::Close(Some(CloseFrame {
                        code: 1003,
                        reason: Cow::Borrowed("Client sent unserializable data"),
                    })))
                    .await,
            );

            drop(sender.close().await);
            None
        }
        Error::InvalidFormat(m) => {
            drop(
                sender
                    .send(Message::Close(Some(CloseFrame {
                        code: 1007,
                        reason: Cow::Owned(m),
                    })))
                    .await,
            );

            drop(sender.close().await);
            None
        }
        Error::Ignore => Some(()),
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
    sender.send(con.encode(OutboundMessage::Hello)).await?;

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
                    handle_error(e, &mut sender).await;

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

    sender
        .send(session.encode(OutboundMessage::Ready {
            session_id: session.id.clone(),
        }))
        .await?;

    while let Ok(Some(mut message)) = receiver.try_next().await {
        if let Message::Close(_) = &message {
            return Ok(());
        }

        let event = session.decode::<InboundMessage>(&mut message);

        if let Ok(event) = event {
            match event {
                InboundMessage::Ping => sender.send(session.encode(OutboundMessage::Pong)).await?,
                _ => {}
            }
        } else if let Err(e) = event {
            if handle_error(e, &mut sender).await.is_some() {
                continue;
            }
        }
    }

    Ok(())
}
