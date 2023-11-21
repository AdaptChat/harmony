use std::{net::IpAddr, time::Duration};

use amqprs::channel::Channel;
use anyhow::{anyhow, bail, Context};
use essence::ws::InboundMessage;
use futures_util::{SinkExt, StreamExt, TryStreamExt};
use tokio_tungstenite::tungstenite::{
    protocol::{frame::coding::CloseCode, CloseFrame},
    Message,
};

use crate::{
    config::{ConnectionSettings, UserSession},
    error::ResultExt,
    presence::{insert_session, PresenceSession, update_presence},
    socket_accept::WebSocketStream,
};

pub async fn process_events(
    websocket: WebSocketStream,
    amqp: Channel,
    ip: IpAddr,
    session: ConnectionSettings,
) -> anyhow::Result<()> {
    let (mut tx, mut rx) = websocket.split();

    let mut hello = tokio::time::timeout(Duration::from_secs(5), rx.try_next())
        .await
        .close_with_context_if_err("timeout", &mut tx)
        .await?
        .close_with_context_if_err("ws error", &mut tx)
        .await?
        .ok_or_else(|| anyhow!("stream closed"))?;

    let hello_event = session.decode::<InboundMessage>(&mut hello);
    if let Err(ref e) = hello_event {
        let _ = tx
            .send(Message::Close(Some(CloseFrame {
                code: CloseCode::Error,
                reason: format!("deser error: {e:?}").into(),
            })))
            .await;
    }
    if let InboundMessage::Identify {
        token,
        status,
        device,
    } = hello_event?
    {
        let session = match UserSession::new(session, token).await {
            Ok(Some(session)) => session,
            Ok(None) => {
                let _ = tx
                    .send(Message::Close(Some(CloseFrame {
                        code: CloseCode::Error,
                        reason: "invalid token".into(),
                    })))
                    .await;
                bail!("invalid token");
            }
            Err(e) => {
                let _ = tx
                    .send(Message::Close(Some(CloseFrame {
                        code: CloseCode::Error,
                        reason: format!("db error: {e:?}").into(),
                    })))
                    .await;
                bail!("invalid token");
            }
        };

        if let Err(e) = insert_session(
            session.user_id,
            PresenceSession {
                session_id: session.get_session_id_str().to_string(),
                online_since: chrono::Utc::now(),
                device,
            },
        )
        .await
        {
            let _ = tx
                .send(Message::Close(Some(CloseFrame {
                    code: CloseCode::Error,
                    reason: format!("redis error: {e:?}").into(),
                })))
                .await;

            Err(e)?;
        }

        if let Err(e) = update_presence(session.user_id, status).await {
            let _ = tx.send(Message::Close(Some(CloseFrame {
                code: CloseCode::Error,
                reason: format!("redis error: {e:?}").into(),
            })))
            .await;
        }

        // TODO: announce presence
    } else {
        let _ = tx
            .send(Message::Close(Some(CloseFrame {
                code: CloseCode::Policy,
                reason: format!("expected `identify` event").into(),
            })))
            .await;
    }

    Ok(())
}
