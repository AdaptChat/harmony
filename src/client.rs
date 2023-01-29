use std::{borrow::Cow, net::IpAddr, num::NonZeroU32, time::Duration};

use essence::ws::{InboundMessage, OutboundMessage};
use flume::Sender;
use futures_util::{stream::SplitStream, TryStreamExt};
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
