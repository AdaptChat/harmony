use std::{borrow::Cow, net::IpAddr, sync::Arc, time::Duration};

use chrono::Utc;
use deadpool_lapin::Object;
use essence::ws::{InboundMessage, OutboundMessage};

use futures_util::{stream::StreamExt, Future, SinkExt, TryStreamExt};

use tokio::{net::TcpStream, sync::{Notify, broadcast::Receiver}};
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
    presence::{insert_session, remove_session, PresenceSession},
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

    let (session, guilds, hidden_channels) = {
        if let Ok(Ok(Some(mut message))) =
            tokio::time::timeout(Duration::from_secs(10), receiver.try_next()).await
        {
            if let Message::Close(_) = &message {
                return Ok(());
            }

            let event = con.decode::<InboundMessage>(&mut message);

            match event {
                Ok(InboundMessage::Identify { token }) => {
                    debug!("initial identify: received identify");

                    match UserSession::new_with_token(con, token).await {
                        Ok(val) => val,
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
        },
    )
    .await?;
    info!("Inserted {0} into online sessions, {0} is now online", session.user_id);

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
        let r = match session.get_ready_event(guilds).await {
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
        let (shutdown, is_shutdown) = tokio::sync::broadcast::channel(3);

        async fn wrap<T>(f: impl Future<Output = Result<T>>, mut shutdown: Receiver<()>) {
            tokio::select! {
                r = f => {
                    match r {
                        Ok(_) => panic!(),
                        Err(e) => {
                            error!("{e}");
                            panic!("{e}");
                        }
                    }
                }
                _ = shutdown.recv() => panic!()
            }
        }

        let r = tokio::try_join!(
            tokio::spawn(wrap(rx_task(), is_shutdown)),
            tokio::spawn(wrap(upstream_task, shutdown.subscribe())),
            tokio::spawn(wrap(client_task, shutdown.subscribe()))
        );

        if let Err(e) = shutdown.send(()) {
            error!("No active Receiver?: {e:?}");
        }

        r?;

        Ok::<(), Error>(())
    }
    .await;

    remove_session(session.user_id, session.id).await?;
    info!("Removed {0} from online sessions, {0} is now offline", session.user_id);

    inner?;

    Ok(())
}
