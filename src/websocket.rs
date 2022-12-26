use std::{borrow::Cow, time::Duration};

use axum::extract::ws::{CloseFrame, Message, WebSocket};
use essence::ws::{InboundMessage, OutboundMessage};
use futures_util::{
    stream::{SplitSink, StreamExt},
    SinkExt, TryStreamExt,
};

use crate::{
    config::{ConnectionConfig, UserSession},
    error::Error,
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

pub async fn handle_socket(socket: WebSocket, con_config: ConnectionConfig) {
    let (mut sender, mut receiver) = socket.split();
    let session = {
        if let Ok(Ok(Some(mut message))) = tokio::time::timeout(Duration::from_secs(10), receiver.try_next()).await {
            match &message {
                Message::Close(_) => drop(sender.close().await),
                _ => {}
            }

            let event = con_config.decode::<InboundMessage>(&mut message);

            if let Ok(InboundMessage::Identify { token }) = event {
                UserSession::new(con_config, token)
            } else if let Err(e) = event {
                handle_error(e, &mut sender).await;

                return;
            } else {
                drop(
                    sender
                        .send(Message::Close(Some(CloseFrame {
                            code: 1003,
                            reason: Cow::Borrowed("Failed to send Identify event"),
                        })))
                        .await,
                );

                drop(sender.close().await);

                return;
            }
        } else {
            drop(
                sender
                    .send(Message::Close(Some(CloseFrame {
                        code: 1003,
                        reason: Cow::Borrowed("Failed to send Identify event"),
                    })))
                    .await,
            );

            drop(sender.close().await);

            return;
        }
    };

    while let Ok(Some(mut message)) = receiver.try_next().await {
        match &message {
            Message::Close(_) => {
                sender.close().await;
                return;
            },
            _ => {}
        }

        let event = session.decode::<InboundMessage>(&mut message);

        if let Ok(event) = event {
            match event {
                InboundMessage::Ping => {
                    if sender.send(session.encode(OutboundEvent::Pong)).await.is_err() {
                        sender.close().await;
                        return;
                    }
                },
                _ => {}
            }
        } else if let Err(e) = event {
            if handle_error(e, &mut sender).await.is_some() {
                continue;
            }
        }
    }
}
