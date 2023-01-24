use std::{borrow::Cow, net::IpAddr, num::NonZeroU32, sync::Arc, time::Duration};

use chrono::Utc;
use deadpool_lapin::Object;
use essence::ws::{InboundMessage, OutboundMessage};
use flume::Sender;
use futures_util::{stream::StreamExt, SinkExt, TryStreamExt};
use governor::{Quota, RateLimiter};
use tokio::{net::TcpStream, sync::Notify};
use tokio_tungstenite::{
    tungstenite::{
        protocol::{frame::coding::CloseCode, CloseFrame},
        Message,
    },
    WebSocketStream,
};

use crate::{
    config::{Connection, UserSession},
    error::{Error, Result},
    presence::{insert_session, remove_session, PresenceSession},
    upstream::handle_upstream,
};

const SECS_10: Duration = Duration::from_secs(10);
const SECS_30: Duration = Duration::from_secs(30);
const UNSUPPORTED: CloseCode = CloseCode::Unsupported;

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

macro_rules! close_if_error {
    ($func:expr, $sender:expr) => {{
        match $func {
            Ok(val) => val,
            Err(e) => return handle_error(e, $sender),
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
    socket: WebSocketStream<TcpStream>,
    con: Connection,
    ip: IpAddr,
    amqp: Object,
) -> Result<()> {
    debug!("Connected from: {ip}");

    let (mut sender, mut receiver) = socket.split();
    sender.send(con.encode(&OutboundMessage::Hello)).await?;

    let (session, guilds) = {
        if let Ok(Ok(Some(mut message))) = tokio::time::timeout(SECS_10, receiver.try_next()).await
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
                                    code: UNSUPPORTED,
                                    reason: Cow::Owned(e.to_string()),
                                })))
                                .await?);
                        }
                    }
                }
                Err(e) => {
                    return Ok(sender
                        .send(Message::Close(Some(CloseFrame {
                            code: UNSUPPORTED,
                            reason: Cow::Owned(e.to_string()),
                        })))
                        .await?)
                }
                _ => {
                    sender
                        .send(Message::Close(Some(CloseFrame {
                            code: UNSUPPORTED,
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
                    code: UNSUPPORTED,
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
            session.clone(),
            tx.clone(),
            amqp,
            upstream_finished_setup.clone(),
            ip,
        );

        const NON_ZERO_1000: NonZeroU32 = unsafe { NonZeroU32::new_unchecked(1000) };
        let ratelimiter = RateLimiter::direct(Quota::per_minute(NON_ZERO_1000));

        let tx_s = tx.clone();
        let ready_event =
            session.encode(&close_if_error!(session.get_ready_event(guilds).await, &tx));

        tokio::spawn(async move || -> Result<()> {
            upstream_finished_setup.notified().await;
            tx_s.send(ready_event)?;

            Ok(())
        }());

        let client_task = async move || -> Result<()> {
            while let Ok(Ok(Some(mut message))) =
                tokio::time::timeout(SECS_30, receiver.try_next()).await
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
                        code: CloseCode::Policy,
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
                        if handle_error(e, &tx).is_ok() {
                            continue;
                        }

                        return Ok(());
                    }
                }
            }

            Ok(())
        };

        tokio::try_join!(rx_task(), upstream_task, client_task())?;

        Ok(())
    }
    .await;

    remove_session(session.user_id, session.id).await?;
    inner?;

    Ok(())
}
