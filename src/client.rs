use std::{borrow::Cow, net::IpAddr, num::NonZeroU32, time::Duration};

use essence::{
    db::{get_pool, GuildDbExt},
    models::Presence,
    ws::{InboundMessage, OutboundMessage},
};
use flume::Sender;
use futures_util::{stream::SplitStream, TryStreamExt, future::JoinAll};
use governor::{Quota, RateLimiter};
use tokio::net::TcpStream;
use tokio_tungstenite::{
    tungstenite::{
        protocol::{frame::coding::CloseCode::Policy, CloseFrame},
        Message,
    },
    WebSocketStream,
};

use crate::{
    config::UserSession,
    error::{Error, Result},
    events::publish_guild_event,
    presence::{get_devices, get_last_session, update_presence},
};

fn handle_error(e: Error, tx: &Sender<Message>) -> Result<()> {
    match e {
        Error::Close(e) => {
            tx.send(Message::Close(Some(CloseFrame {
                code: 1003.into(),
                reason: Cow::Owned(e),
            })))?;

            Err(Error::Ignore)
        }
        Error::Ignore => Ok(()),
    }
}

pub async fn client_rx(
    mut receiver: SplitStream<WebSocketStream<TcpStream>>,
    tx: Sender<Message>,
    session: UserSession,
    ip: IpAddr,
) -> Result<()> {
    let ratelimiter = RateLimiter::direct(Quota::per_minute(unsafe {
        NonZeroU32::new_unchecked(1000)
    }));

    while let Ok(Ok(Some(mut message))) =
        tokio::time::timeout(Duration::from_secs(30), receiver.try_next()).await
    {
        if let Message::Close(_) = &message {
            return Ok(());
        }

        if ratelimiter.check().is_err() {
            info!(
                "Rate limit exceeded for {ip} - {}, disconnecting",
                &session.id
            );

            tx.send(Message::Close(Some(CloseFrame {
                code: Policy,
                reason: Cow::Borrowed("Rate limit exceeded"),
            })))?;

            return Ok(());
        }

        let event = session.decode::<InboundMessage>(&mut message);

        match event {
            Ok(event) => match event {
                InboundMessage::Ping => tx.send(session.encode(&OutboundMessage::Pong))?,
                InboundMessage::UpdatePresence { status } => {
                    if let Some(status) = status {
                        update_presence(session.user_id, status).await?;
                        get_pool()
                            .fetch_all_guild_ids_for_user(session.user_id)
                            .await?
                            .into_iter()
                            .map(|g| async move {
                                publish_guild_event(
                                    g,
                                    OutboundMessage::PresenceUpdate {
                                        presence: Presence {
                                            user_id: session.user_id,
                                            status,
                                            custom_status: None,
                                            devices: get_devices(session.user_id).await?,
                                            online_since: get_last_session(session.user_id)
                                                .await?
                                                .ok_or_else(|| {
                                                    Error::Close(
                                                        "online_since does not exist".to_string(),
                                                    )
                                                })
                                                .map(|v| v.online_since)?,
                                        },
                                    },
                                ).await?;

                                Ok::<(), Error>(())
                            })
                            .collect::<JoinAll<_>>()
                            .await;
                    }
                }
                _ => {}
            },
            Err(e) => {
                if handle_error(e, &tx).is_ok() {
                    continue;
                }

                return Ok(());
            }
        }
    }

    Ok(())
}
