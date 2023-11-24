use std::{net::IpAddr, time::Duration};

use ahash::{HashSet, HashSetExt};
use amqprs::channel::{
    BasicConsumeArguments, Channel, ConsumerMessage, QueueBindArguments, QueueDeclareArguments,
};
use essence::{
    calculate_permissions_sorted,
    db::{get_pool, ChannelDbExt, GuildDbExt, UserDbExt},
    http::guild::GetGuildQuery,
    models::{Channel as EssenceChannel, Permissions, Presence},
    ws::{InboundMessage, OutboundMessage},
};
use futures_util::{SinkExt, StreamExt, TryStreamExt};
use tokio::sync::Mutex;
use tokio_tungstenite::tungstenite::{
    protocol::{frame::coding::CloseCode, CloseFrame},
    Message,
};

use crate::{
    bail, bail_with_ctx,
    config::{ConnectionSettings, UserSession},
    err_with_ctx,
    error::Result,
    events::{subscribe, unsubscribe, CONFIG},
    presence::{
        get_devices, get_first_session, get_presence, insert_session, publish_presence_change,
        remove_session, update_presence, PresenceSession,
    },
    socket_accept::WebSocketStream,
};

pub async fn process_events(
    websocket: WebSocketStream,
    amqp: Channel,
    ip: IpAddr,
    settings: ConnectionSettings,
) -> Result<()> {
    let (tx, mut rx) = websocket.split();
    let tx = Mutex::new(tx);

    if let Err(e) = tx
        .lock()
        .await
        .send(settings.encode(&OutboundMessage::Hello))
        .await
    {
        // can't send anything to client, which also applies to close message
        bail_with_ctx!(e, "failed to send hello event: tx.send");
    }

    let hello_event = {
        if let Ok(Ok(Some(mut hello))) =
            tokio::time::timeout(Duration::from_secs(5), rx.try_next()).await
        {
            let hello_event = settings.decode::<InboundMessage>(&mut hello);
            match hello_event {
                Ok(hello_event) => hello_event,
                Err(e) => {
                    let _ = tx
                        .lock()
                        .await
                        .send(Message::Close(Some(CloseFrame {
                            code: CloseCode::Error,
                            reason: format!("deser error: {e:?}").into(),
                        })))
                        .await;
                    bail_with_ctx!(e, "deserialize hello event: settings.decode");
                }
            }
        } else {
            let _ = tx
                .lock()
                .await
                .send(Message::Close(Some(CloseFrame {
                    code: CloseCode::Policy,
                    reason: "expected to receive `identify` event within 5 seconds".into(),
                })))
                .await;

            return Err(crate::error::Error::default()
                .ctx("failed to receive `identify` event within 5 seconds"));
        }
    };

    if let InboundMessage::Identify {
        token,
        status,
        device,
    } = hello_event
    {
        let session = match UserSession::new(settings, token).await {
            Ok(Some(session)) => session,
            Ok(None) => {
                let _ = tx
                    .lock()
                    .await
                    .send(Message::Close(Some(CloseFrame {
                        code: CloseCode::Error,
                        reason: "invalid token".into(),
                    })))
                    .await;
                bail!("invalid token")
            }
            Err(e) => {
                let _ = tx
                    .lock()
                    .await
                    .send(Message::Close(Some(CloseFrame {
                        code: CloseCode::Error,
                        reason: format!("db error: {e:?}").into(),
                    })))
                    .await;
                bail!("invalid token");
            }
        };

        let inner = async {
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
                    .lock()
                    .await
                    .send(Message::Close(Some(CloseFrame {
                        code: CloseCode::Error,
                        reason: format!("redis error: {e:?}").into(),
                    })))
                    .await;

                bail_with_ctx!(e, "insert_session");
            }

            if let Err(e) = update_presence(session.user_id, status).await {
                bail_with_ctx!(e, "update_presence");
            }

            info!("publishing presence change");
            if let Err(e) = publish_presence_change(
                &amqp,
                session.user_id,
                Presence {
                    user_id: session.user_id,
                    status,
                    custom_status: None,
                    devices: get_devices(session.user_id).await?, // TODO: Err
                    online_since: Some(
                        get_first_session(session.user_id)
                            .await?
                            .map_or_else(|| online_since, |s| s.online_since),
                    ),
                },
            )
            .await
            {
                bail_with_ctx!(e, "publish_presence_change");
            }

            info!("published user {}'s presence.", session.user_id);

            let presences = {
                let users = get_pool()
                    .fetch_observable_user_ids_for_user(session.user_id)
                    .await
                    .map_err(|e| {
                        err_with_ctx!(e, "fetch presences: fetch_observable_user_ids_for_user")
                    })?;
                let mut presences = Vec::with_capacity(users.len());

                for user_id in users {
                    presences.push(Presence {
                        user_id,
                        status: get_presence(user_id).await?,
                        custom_status: None,
                        devices: get_devices(user_id).await?,
                        online_since: get_first_session(user_id)
                            .await?
                            .map_or_else(|| None, |s| Some(s.online_since)),
                    });
                }

                presences
            };

            match session.get_ready_event(presences).await {
                Ok(ready) => {
                    if let Err(e) = tx.lock().await.send(session.encode(&ready)).await {
                        bail_with_ctx!(e, "send ready event: tx.send");
                    }
                }
                Err(e) => {
                    bail_with_ctx!(e, "generate ready event: session.get_ready_event");
                }
            }

            if let Err(e) = amqp
                .queue_declare(QueueDeclareArguments::transient_autodelete(
                    session.get_session_id_str(),
                ))
                .await
            {
                bail_with_ctx!(e, "declare queue: queue_declare");
            }

            match get_pool()
                .fetch_all_guild_ids_for_user(session.user_id)
                .await
            {
                Ok(guilds) => {
                    for guild in guilds {
                        if let Err(e) =
                            subscribe(&amqp, guild, session.get_session_id_str(), "topic").await
                        {
                            bail_with_ctx!(e, "subscribe to guilds: subscribe");
                        }
                    }
                }
                Err(e) => {
                    bail_with_ctx!(e, "fetch guild ids: fetch_all_guild_ids_for_user");
                }
            }

            match get_pool()
                .fetch_all_dm_channels_for_user(session.user_id)
                .await
            {
                Ok(dm_channels) => {
                    for channel in dm_channels {
                        if let Err(e) =
                            subscribe(&amqp, channel.id, session.get_session_id_str(), "topic")
                                .await
                        {
                            bail_with_ctx!(e, "subscribe to dm channels: subscribe");
                        }
                    }
                }
                Err(e) => {
                    bail_with_ctx!(e, "fetch dm channels: fetch_all_dm_channels_for_user");
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
                bail_with_ctx!(e, "bind queue: queue_bind");
            }

            let (_, mut amqp_rx) = match amqp
                .basic_consume_rx(
                    BasicConsumeArguments::new(
                        session.get_session_id_str(),
                        &format!(
                            "consumer-{}-{}-{}",
                            session.user_id,
                            session.get_session_id_str(),
                            ip
                        ),
                    )
                    .finish(),
                )
                .await
            {
                Ok(r) => r,
                Err(e) => {
                    bail_with_ctx!(e, "channel consume: basic_consume_rx");
                }
            };

            let guilds = match get_pool()
                .fetch_all_guilds_for_user(
                    session.user_id,
                    GetGuildQuery {
                        channels: true,
                        roles: true,
                        ..Default::default()
                    },
                )
                .await
            {
                Ok(guilds) => guilds,
                Err(e) => bail_with_ctx!(e, "create hidden_channels: fetch_all_guilds_for_user"),
            };

            let mut hidden_channels = {
                let mut hidden = HashSet::new();

                for guild in guilds {
                    if guild.partial.owner_id == session.user_id {
                        continue;
                    }

                    if let Some(channels) = guild.channels {
                        if channels.is_empty() {
                            continue;
                        }

                        let mut roles = guild.roles.unwrap_or_default();
                        roles.sort_by_key(|r| r.position);

                        for channel in channels {
                            let perm = calculate_permissions_sorted(
                                session.user_id,
                                &roles,
                                Some(&channel.overwrites),
                            );

                            if !perm.contains(Permissions::VIEW_CHANNEL) {
                                hidden.insert(channel.id);
                            }
                        }
                    }
                }

                hidden
            };

            let upstream_listener = async {
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
                                    subscribe(&amqp, chan.id, session.get_session_id_str(), "topic")
                                        .await
                                {
                                    error!("failed to subscribe to amqp exchange: {e:?}");
                                    break;
                                }
                            }
                            OutboundMessage::ChannelCreate {
                                channel: EssenceChannel::Guild(chan),
                            } => {
                                match get_pool()
                                    .fetch_guild(
                                        chan.guild_id,
                                        GetGuildQuery {
                                            roles: true,
                                            ..Default::default()
                                        },
                                    )
                                    .await
                                {
                                    Ok(Some(guild)) => {
                                        if guild.partial.owner_id != session.user_id {
                                            let mut roles = guild.roles.unwrap_or_default();
                                            roles.sort_by_key(|r| r.position);

                                            let perm = calculate_permissions_sorted(
                                                session.user_id,
                                                &roles,
                                                Some(&chan.overwrites),
                                            );

                                            if !perm.contains(Permissions::VIEW_CHANNEL) {
                                                hidden_channels.insert(chan.id);
                                                continue;
                                            }
                                        }
                                    }
                                    Ok(None) => {
                                        warn!("guild not found after channel create?");
                                        break;
                                    }
                                    Err(e) => {
                                        error!("failed to fetch guild: {e:?}");
                                        break;
                                    }
                                }
                            }
                            OutboundMessage::ChannelUpdate {
                                after: EssenceChannel::Guild(after),
                                ..
                            } => {
                                match get_pool()
                                    .fetch_guild(
                                        after.guild_id,
                                        GetGuildQuery {
                                            roles: true,
                                            ..Default::default()
                                        },
                                    )
                                    .await
                                {
                                    Ok(Some(guild)) => {
                                        if guild.partial.owner_id != session.user_id {
                                            let mut roles = guild.roles.unwrap_or_default();
                                            roles.sort_by_key(|r| r.position);

                                            let perm = calculate_permissions_sorted(
                                                session.user_id,
                                                &roles,
                                                Some(&after.overwrites),
                                            );

                                            if !perm.contains(Permissions::VIEW_CHANNEL) {
                                                hidden_channels.insert(after.id);
                                                continue;
                                            } else {
                                                hidden_channels.remove(&after.id);
                                            }
                                        }
                                    }
                                    Ok(None) => {
                                        warn!("guild not found after channel update?");
                                        break;
                                    }
                                    Err(e) => {
                                        error!("failed to fetch guild: {e:?}");
                                        break;
                                    }
                                }
                            }
                            OutboundMessage::ChannelDelete { channel_id } => {
                                if let Err(e) =
                                    unsubscribe(&amqp, channel_id, session.get_session_id_str())
                                        .await
                                {
                                    error!("failed to unsubscribe to amqp exchange: {e:?}");
                                    break;
                                }
                            }
                            OutboundMessage::GuildCreate { guild, .. } => {
                                if let Err(e) = subscribe(
                                    &amqp,
                                    guild.partial.id,
                                    session.get_session_id_str(),
                                    "topic",
                                )
                                .await
                                {
                                    error!("failed to subscribe to amqp exchange: {e:?}");
                                    break;
                                }
                            }
                            OutboundMessage::GuildRemove { guild_id, .. } => {
                                if let Err(e) =
                                    unsubscribe(&amqp, guild_id, session.get_session_id_str()).await
                                {
                                    error!("failed to unsubscribe to amqp exchange: {e:?}");
                                    break;
                                }
                            }
                            OutboundMessage::MessageCreate { message, .. }
                            | OutboundMessage::MessageUpdate { after: message, .. } => {
                                if hidden_channels.contains(&message.channel_id) {
                                    continue;
                                }
                            }
                            OutboundMessage::RoleCreate { role }
                            | OutboundMessage::RoleUpdate { after: role, .. } => {
                                match get_pool()
                                    .fetch_guild(
                                        role.guild_id,
                                        GetGuildQuery {
                                            roles: true,
                                            channels: true,
                                            ..Default::default()
                                        },
                                    )
                                    .await
                                {
                                    Ok(Some(guild)) => {
                                        if guild.partial.owner_id != session.user_id {
                                            if let Some(channels) = guild.channels {
                                                if channels.is_empty() {
                                                    continue;
                                                }

                                                let mut roles = guild.roles.unwrap_or_default();
                                                roles.sort_by_key(|r| r.position);

                                                for channel in channels {
                                                    let perm = calculate_permissions_sorted(
                                                        session.user_id,
                                                        &roles,
                                                        Some(&channel.overwrites),
                                                    );

                                                    if !perm.contains(Permissions::VIEW_CHANNEL) {
                                                        hidden_channels.insert(channel.id);
                                                        continue;
                                                    }
                                                }
                                            }
                                        }
                                    }
                                    Ok(None) => {
                                        warn!("guild not found after role create/update?");
                                        break;
                                    }
                                    Err(e) => {
                                        error!("failed to fetch guild: {e:?}");
                                        break;
                                    }
                                }
                            }
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

                                if let Err(e) = publish_presence_change(
                                    &amqp,
                                    session.user_id,
                                    Presence {
                                        user_id: session.user_id,
                                        status,
                                        custom_status: None,
                                        devices: match get_devices(session.user_id).await {
                                            Ok(devices) => devices,
                                            Err(e) => {
                                                error!("redis error in get_devices: {e:?}");
                                                break;
                                            }
                                        },
                                        online_since: match get_first_session(session.user_id).await
                                        {
                                            Ok(devices) => devices
                                                .map_or_else(|| None, |s| Some(s.online_since)),
                                            Err(e) => {
                                                error!("redis error in get_first_session: {e:?}");
                                                break;
                                            }
                                        },
                                    },
                                )
                                .await
                                {
                                    error!("error while publish presence change: {e:?}");
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

            Ok(())
        }
        .await;

        let cleanup_succeeded = {
            let res = remove_session(session.user_id, session.get_session_id_str()).await;
            let res_amqp = amqp.close().await;

            res.is_ok() && res_amqp.is_ok()
        };

        if let Err(e) = inner {
            if let Ok(ref mut tx) = tx.try_lock() {
                let _ = tx
                    .send(Message::Close(Some(CloseFrame {
                        code: CloseCode::Abnormal,
                        reason: e.to_string().into(),
                    })))
                    .await;
            }
            error!(
                "session {} errored: {e}, cleanup succeeded: {cleanup_succeeded}",
                session.get_session_id_str()
            );
        } else {
            if let Ok(ref mut tx) = tx.try_lock() {
                let _ = tx
                    .send(Message::Close(Some(CloseFrame {
                        code: CloseCode::Normal,
                        reason: "client disconnected".into(),
                    })))
                    .await;
            }

            info!(
                "session {} disconnected, cleanup succeeded: {cleanup_succeeded}",
                session.get_session_id_str()
            );
        }
    } else {
        let _ = tx
            .lock()
            .await
            .send(Message::Close(Some(CloseFrame {
                code: CloseCode::Policy,
                reason: format!("expected `identify` event").into(),
            })))
            .await;
    }

    Ok(())
}
