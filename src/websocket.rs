use std::{net::IpAddr, time::Duration};

use amqprs::channel::{
    BasicConsumeArguments, Channel, ConsumerMessage, QueueBindArguments, QueueDeclareArguments,
};
use anyhow::{anyhow, bail};
use essence::{
    db::{get_pool, ChannelDbExt, GuildDbExt, UserDbExt},
    models::{Channel as EssenceChannel, Presence},
    ws::{InboundMessage, OutboundMessage},
};
use futures_util::{SinkExt, StreamExt, TryStreamExt};
use tokio::sync::Mutex;
use tokio_tungstenite::tungstenite::{
    protocol::{frame::coding::CloseCode, CloseFrame},
    Message,
};

use crate::{
    config::{ConnectionSettings, UserSession},
    error::ResultExt,
    events::{subscribe, unsubscribe, CONFIG},
    presence::{
        get_devices, get_last_session, get_presence, insert_session, publish_presence_change,
        remove_session, update_presence, PresenceSession,
    },
    socket_accept::WebSocketStream,
};

pub async fn process_events(
    websocket: WebSocketStream,
    amqp: Channel,
    ip: IpAddr,
    settings: ConnectionSettings,
) -> anyhow::Result<()> {
    let (mut tx, mut rx) = websocket.split();

    let mut hello = tokio::time::timeout(Duration::from_secs(5), rx.try_next())
        .await
        .close_with_context_if_err("timeout", &mut tx)
        .await?
        .close_with_context_if_err("ws error", &mut tx)
        .await?
        .ok_or_else(|| anyhow!("stream closed"))?;

    let hello_event = settings.decode::<InboundMessage>(&mut hello);
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
        let session = match UserSession::new(settings, token).await {
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

        let online_since = chrono::Utc::now();

        if let Err(e) = insert_session(
            session.user_id,
            PresenceSession {
                session_id: session.get_session_id_str().to_string(),
                online_since,
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
            let _ = tx
                .send(Message::Close(Some(CloseFrame {
                    code: CloseCode::Error,
                    reason: format!("redis error: {e:?}").into(),
                })))
                .await;
            Err(e)?;
        }

        if let Err(e) = publish_presence_change(
            session.user_id,
            Presence {
                user_id: session.user_id,
                status,
                custom_status: None,
                devices: get_devices(session.user_id).await?, // TODO: Err
                online_since: Some(
                    get_last_session(session.user_id)
                        .await?
                        .map_or_else(|| online_since, |s| s.online_since),
                ),
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

        info!("published user {}'s presence.", session.user_id);

        let presences = {
            let users = get_pool()
                .fetch_observable_user_ids_for_user(session.user_id)
                .await
                .map_err(|e| anyhow!(format!("{e:?}")))?; // TODO: err
            let mut presences = Vec::with_capacity(users.len());

            for user_id in users {
                presences.push(Presence {
                    user_id,
                    status: get_presence(user_id).await?,
                    custom_status: None,
                    devices: get_devices(user_id).await?,
                    online_since: get_last_session(user_id)
                        .await?
                        .map_or_else(|| None, |s| Some(s.online_since)),
                });
            }

            presences
        };

        match session.get_ready_event(presences).await {
            Ok(ready) => {
                if let Err(e) = tx.send(session.encode(&ready)).await {
                    let _ = tx
                        .send(Message::Close(Some(CloseFrame {
                            code: CloseCode::Error,
                            reason: format!("ws send failed: {e:?}").into(),
                        })))
                        .await;
                    Err(e)?
                }
            }
            Err(e) => {
                let _ = tx
                    .send(Message::Close(Some(CloseFrame {
                        code: CloseCode::Error,
                        reason: format!("failed to generate ready event: {e:?}").into(),
                    })))
                    .await;
                return Err(anyhow!("essence error: {e:?}"));
            }
        }
        if let Err(e) = amqp
            .queue_declare(QueueDeclareArguments::transient_autodelete(
                session.get_session_id_str(),
            ))
            .await
        {
            error!("failed to declare queue: {e:?}");

            let _ = tx
                .send(Message::Close(Some(CloseFrame {
                    code: CloseCode::Error,
                    reason: format!("amqp error: {e:?}").into(),
                })))
                .await;

            Err(e)?
        }

        match get_pool()
            .fetch_all_guild_ids_for_user(session.user_id)
            .await
        {
            Ok(guilds) => {
                for guild in guilds {
                    if let Err(e) = subscribe(&amqp, guild, session.session_id, "topic").await {
                        error!("amqp subscribe failed: {e:?}");
                        Err(e)?;
                    }
                }
            }
            Err(e) => {
                error!("failed to fetch guilds: {e:?}");
                bail!("essence error: {e:?}");
            }
        }

        match get_pool()
            .fetch_all_dm_channels_for_user(session.user_id)
            .await
        {
            Ok(dm_channels) => {
                for channel in dm_channels {
                    if let Err(e) = subscribe(&amqp, channel.id, session.session_id, "topic").await
                    {
                        error!("amqp subscribe failed: {e:?}");
                        Err(e)?;
                    }
                }
            }
            Err(e) => {
                error!("failed to fetch dm channels: {e:?}");
                bail!("essence error: {e:?}");
            }
        }

        if let Err(e) = amqp
            .queue_bind(QueueBindArguments {
                queue: session.get_session_id_str().to_string(),
                exchange: "events".to_string(),
                routing_key: session.user_id.to_string(),
                ..Default::default()
            })
            .await
        {
            error!("failed to declare queue: {e:?}");

            let _ = tx
                .send(Message::Close(Some(CloseFrame {
                    code: CloseCode::Error,
                    reason: format!("amqp error: {e:?}").into(),
                })))
                .await;

            Err(e)?
        }

        let (_, mut amqp_rx) = match amqp
            .basic_consume_rx(
                BasicConsumeArguments::new(
                    session.get_session_id_str(),
                    &format!("consumer-{}-{}-{}", session.user_id, session.session_id, ip),
                )
                .finish(),
            )
            .await
        {
            Ok(r) => r,
            Err(e) => {
                error!("amqp basic consume failed: {e:?}");

                let _ = tx
                    .send(Message::Close(Some(CloseFrame {
                        code: CloseCode::Error,
                        reason: format!("amqp error: {e:?}").into(),
                    })))
                    .await;

                bail!("amqp basic consume failed: {e:?}");
            }
        };

        let tx = Mutex::new(tx);

        let upstream_listener = async {
            // TODO: Hidden channels
            while let Some(ConsumerMessage {
                content: Some(content),
                ..
            }) = amqp_rx.recv().await
            {
                if let Ok((event, _)) =
                    bincode::decode_from_slice::<OutboundMessage, _>(&content, CONFIG)
                {
                    match &event {
                        OutboundMessage::ChannelCreate {
                            channel: EssenceChannel::Dm(chan),
                        } => {
                            if let Err(e) =
                                subscribe(&amqp, chan.id, session.session_id, "topic").await
                            {
                                error!("failed to subscribe to amqp exchange: {e:?}");
                                break;
                            }
                        }
                        OutboundMessage::ChannelDelete { channel_id } => {
                            if let Err(e) = unsubscribe(&amqp, channel_id, session.session_id).await
                            {
                                error!("failed to unsubscribe to amqp exchange: {e:?}");
                                break;
                            }
                        }
                        OutboundMessage::GuildCreate { guild, .. } => {
                            if let Err(e) =
                                subscribe(&amqp, guild.partial.id, session.session_id, "topic")
                                    .await
                            {
                                error!("failed to subscribe to amqp exchange: {e:?}");
                                break;
                            }
                        }
                        OutboundMessage::GuildRemove { guild_id, .. } => {
                            if let Err(e) = unsubscribe(&amqp, guild_id, session.session_id).await {
                                error!("failed to unsubscribe to amqp exchange: {e:?}");
                                break;
                            }
                        }
                        // TODO: Channels
                        _ => {}
                    }
                    if let Err(e) = tx.lock().await.send(session.encode(&event)).await {
                        debug!("failed to send to client: {e:?}");
                        break;
                    }
                }
            }
        };

        let ws_listener = async {
            while let Ok(Some(mut msg)) = rx.try_next().await {
                if let Ok(incoming) = session.decode::<InboundMessage>(&mut msg) {
                    match incoming {
                        InboundMessage::Ping => {
                            if let Err(e) = tx
                                .lock()
                                .await
                                .send(session.encode(&OutboundMessage::Pong))
                                .await
                            {
                                warn!("failed to send: {e:?}");
                                break;
                            }
                        }
                        InboundMessage::UpdatePresence {
                            status: Some(status),
                        } => {
                            if let Err(e) = update_presence(session.user_id, status).await {
                                error!("failed to update presence, redis error: {e:?}");
                                let _ = tx
                                    .lock()
                                    .await
                                    .send(Message::Close(Some(CloseFrame {
                                        code: CloseCode::Error,
                                        reason: format!("redis error: {e:?}").into(),
                                    })))
                                    .await;
                                break;
                            }
                        }
                        _ => {}
                    }
                }
            }
        };

        tokio::select! {
            _ = upstream_listener => {
                debug!("upstream died");
            },
            _ = ws_listener => {
                debug!("ws_listener died")
            }
        }

        let _ = remove_session(session.user_id, session.get_session_id_str()).await;
        let _ = amqp.close().await;

        if let Ok(ref mut tx) = tx.try_lock() {
            let _ = tx
                .send(Message::Close(Some(CloseFrame {
                    code: CloseCode::Abnormal,
                    reason: "errored".into(),
                })))
                .await;
        };
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
