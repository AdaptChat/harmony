use std::{borrow::Cow, net::IpAddr, sync::Arc, time::Duration};

use chrono::Utc;
use deadpool_lapin::Object;
use essence::{
    db::{get_pool, GuildDbExt, UserDbExt},
    models::{Presence, PresenceStatus, Devices},
    ws::{InboundMessage, OutboundMessage},
};

use futures_util::{stream::StreamExt, Future, SinkExt, TryStreamExt};

use tokio::{net::TcpStream, sync::Notify};
use tokio_tungstenite::{
    tungstenite::{
        protocol::{frame::coding::CloseCode::Unsupported, CloseFrame},
        Message,
    },
    WebSocketStream,
};

use crate::{
    client::client_rx,
    config::{Connection, UserSession},
    error::{Error, Result},
    presence::{
        get_devices, get_last_session, get_presence, insert_session, publish_presence_change,
        remove_session, update_presence, PresenceSession,
    },
    upstream::handle_upstream,
};

/// Process event for a websocket connection.
///
/// # Error
///
/// This function returns an error solely because of the conveience of the ? operator
/// It uses the ? operator on `sender.send` function because if it fails to send that means the client has disconnected, meaning is safe to return
/// The error return by this function is meant to be ignored.
pub async fn handle_socket(
    socket: WebSocketStream<TcpStream>,
    con: Connection,
    ip: IpAddr,
    amqp: Object,
) -> Result<()> {
    debug!("Connected from: {ip}");

    let (mut sender, mut receiver) = socket.split();
    sender.send(con.encode(&OutboundMessage::Hello)).await?;

    let (session, guilds, hidden_channels, status, device) = {
        if let Ok(Ok(Some(mut message))) =
            tokio::time::timeout(Duration::from_secs(10), receiver.try_next()).await
        {
            if let Message::Close(_) = &message {
                return Ok(());
            }

            let event = con.decode::<InboundMessage>(&mut message);

            match event {
                Ok(InboundMessage::Identify {
                    token,
                    status,
                    device,
                }) => {
                    debug!("initial identify: received identify");

                    match UserSession::new_with_token(con, token).await {
                        Ok(val) => (val.0, val.1, val.2, status, device),
                        Err(e) => {
                            return Ok(sender
                                .send(Message::Close(Some(CloseFrame {
                                    code: Unsupported,
                                    reason: Cow::Owned(e.to_string()),
                                })))
                                .await?);
                        }
                    }
                }
                Err(e) => {
                    return Ok(sender
                        .send(Message::Close(Some(CloseFrame {
                            code: Unsupported,
                            reason: Cow::Owned(e.to_string()),
                        })))
                        .await?)
                }
                _ => {
                    sender
                        .send(Message::Close(Some(CloseFrame {
                            code: Unsupported,
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
                    code: Unsupported,
                    reason: Cow::Borrowed("Failed to send Identify event"),
                })))
                .await?;

            sender.close().await?;

            return Ok(());
        }
    };

    insert_session(
        session.user_id,
        PresenceSession {
            session_id: session.id.clone(),
            online_since: Utc::now(),
            device,
        },
    )
    .await?;

    update_presence(session.user_id, status).await?;

    publish_presence_change(
        session.user_id,
        Presence {
            user_id: session.user_id,
            status,
            custom_status: None,
            devices: get_devices(session.user_id).await?,
            online_since: if status == PresenceStatus::Offline {
                None
            } else {
                Some(
                    get_last_session(session.user_id)
                        .await?
                        .expect("Session not found")
                        .online_since,
                )
            },
        },
    )
    .await?;

    let presences = {
        let users = get_pool()
            .fetch_observable_user_ids_for_user(session.user_id)
            .await?;
        let mut presences = Vec::with_capacity(users.len());

        for user_id in users {
            presences.push(Presence {
                user_id,
                status: get_presence(user_id).await?,
                custom_status: None,
                devices: get_devices(user_id).await?,
                online_since: Some(
                    get_last_session(user_id)
                        .await?
                        .map_or_else(Utc::now, |s| s.online_since),
                ),
            });
        }

        presences
    };

    info!(
        "Inserted {0} into online sessions, {0} is now online",
        session.user_id
    );

    let ref_session = &session;

    let inner = async move {
        let session = ref_session;

        let (tx, rx) = flume::unbounded::<Message>();

        let rx_task = async move || -> Result<()> {
            while let Ok(m) = rx.recv_async().await {
                sender.send(m).await?;
            }

            Ok(())
        };

        let upstream_finished_setup = Arc::new(Notify::new());

        let upstream_task = handle_upstream(
            session.user_id.to_string(),
            session.id.clone(),
            tx.clone(),
            amqp,
            upstream_finished_setup.clone(),
            ip,
            hidden_channels,
            guilds.clone(),
            session.format,
        );

        let tx_s = tx.clone();
        let r = match session.get_ready_event(guilds, presences).await {
            Ok(r) => r,
            Err(e) => {
                tx.send(Message::Close(Some(CloseFrame {
                    code: Unsupported,
                    reason: Cow::Owned(e.to_string()),
                })))?;

                return Ok(());
            }
        };

        let ready_event = session.encode(&r);

        tokio::spawn(async move {
            upstream_finished_setup.notified().await;
            tx_s.send(ready_event)?;

            Ok::<(), Error>(())
        });

        let client_task = client_rx(receiver, tx, session.clone(), ip);
        let notified = Arc::new(Notify::new());

        async fn inner_wrap<T>(
            f: impl Future<Output = Result<T>>,
            shutdown: Arc<Notify>,
        ) -> Result<T> {
            tokio::select! {
                r = f => r,
                _ = shutdown.notified() => Err(Error::Ignore)
            }
        }

        let r = tokio::select! {
            r = tokio::spawn(inner_wrap(rx_task(), notified.clone())) => {
                debug!("Exit from `rx_task`: {r:?}");
                r
            },
            r = tokio::spawn(inner_wrap(upstream_task, notified.clone())) => {
                debug!("Exit from `upstream_task`: {r:?}");
                r
            },
            r = tokio::spawn(inner_wrap(client_task, notified.clone())) => {
                debug!("Exit from `client_task`: {r:?}");
                r
            }
        };

        notified.notify_waiters();

        r??;

        Ok::<(), Error>(())
    }
    .await;

    remove_session(session.user_id, session.id).await?;

    if get_devices(session.user_id).await?.is_empty() {
        update_presence(session.user_id, PresenceStatus::Offline).await?;

        publish_presence_change(
            session.user_id,
            Presence {
                user_id: session.user_id,
                status: PresenceStatus::Offline,
                custom_status: None,
                devices: Devices::empty(),
                online_since: None,
            },
        )
        .await?;
    }

    info!(
        "Removed {0} from online sessions, {0} is now offline",
        session.user_id
    );

    inner?;

    Ok(())
}
