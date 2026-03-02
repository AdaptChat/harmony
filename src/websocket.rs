use std::{collections::HashMap, net::IpAddr, time::Duration};

use ahash::{HashSet, HashSetExt};
use amqprs::channel::{
    BasicConsumeArguments, Channel, ConsumerMessage, QueueBindArguments, QueueDeclareArguments,
};
use essence::{
    calculate_permissions, calculate_permissions_sorted,
    db::{get_pool, GuildDbExt, MemberDbExt, UserDbExt},
    http::guild::GetGuildQuery,
    models::{
        Channel as EssenceChannel, Devices, PermissionOverwrite, Permissions, Presence,
        PresenceStatus, Role,
    },
    ws::{InboundMessage, OutboundMessage},
};
use futures_util::{SinkExt, StreamExt};
use tokio::sync::Mutex;
use tokio_tungstenite::tungstenite::{
    protocol::{frame::coding::CloseCode, CloseFrame},
    Message,
};

use crate::{
    bail, bail_with_ctx,
    config::{ConnectionSettings, UserSession},
    err_with_ctx,
    error::{Error, Result},
    events::{publish_guild_event, subscribe, unsubscribe, CONFIG},
    presence::{
        any_session_exists, get_devices, get_first_session, get_presence, get_presences_bulk,
        insert_session, publish_presence_change, remove_session, update_presence, PresenceSession,
    },
    socket_accept::WebSocketStream,
};

async fn update_hidden_channels(
    guild_id: u64,
    user_id: u64,
    channel_id: u64,
    hidden_channels: &mut HashSet<u64>,
    roles: impl AsMut<[Role]>,
    overwrites: Option<&[PermissionOverwrite]>,
) -> Result<()> {
    let member = get_pool()
        .fetch_member_by_id(guild_id, user_id)
        .await?
        .ok_or("member not found")?;

    let perm = calculate_permissions(user_id, member.permissions, roles, overwrites);

    if perm.contains(Permissions::VIEW_CHANNEL) {
        hidden_channels.remove(&channel_id);
    } else {
        hidden_channels.insert(channel_id);
    }

    Ok(())
}

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

    let identify = {
        if let Ok(Some(Ok(mut message))) =
            tokio::time::timeout(Duration::from_secs(5), rx.next()).await
        {
            let identify = settings.decode::<InboundMessage>(&mut message);
            match identify {
                Ok(identify) => identify,
                Err(e) => {
                    let _ = tx
                        .lock()
                        .await
                        .send(Message::Close(Some(CloseFrame {
                            code: CloseCode::Error,
                            reason: format!("deser error: {e:?}").into(),
                        })))
                        .await;
                    bail_with_ctx!(e, "deserialize identify event: settings.decode");
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

            return Err(Error::default().ctx("failed to receive `identify` event within 5 seconds"));
        }
    };

    if let InboundMessage::Identify {
        token,
        status,
        custom_status,
        device,
        ..
    } = identify
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

            if let Some(status) = &custom_status {
                if status.len() > 256 {
                    if let Err(e) = tx
                        .lock()
                        .await
                        .send(Message::Close(
                            Some(CloseFrame {
                                code: CloseCode::Policy,
                                reason: "status length exceeds 256 characters".into() 
                        }))).await {
                            warn!("failed to send: {e:?}");
                    }

                    return Err("status length exceeds 256 characters".into());
                }
            }

            if let Err(e) = update_presence(session.user_id, status, custom_status.clone()).await {
                bail_with_ctx!(e, "update_presence");
            }

            info!("publishing presence change");
            let presence = Presence {
                user_id: session.user_id,
                status,
                custom_status,
                devices: get_devices(session.user_id).await?, // TODO: Err
                online_since: Some(
                    get_first_session(session.user_id)
                        .await?
                        .map_or_else(|| online_since, |s| s.online_since),
                ),
            };

            let mut observable = get_pool()
                .fetch_observable_user_ids_for_user(session.user_id)
                .await
                .map_err(|e| {
                    err_with_ctx!(e, "fetch presences: fetch_observable_user_ids_for_user")
                })?;
            observable.retain(|&id| id != session.user_id);

            if let Err(e) = publish_presence_change(
                &amqp,
                session.user_id,
                presence.clone(),
                &observable,
            )
            .await
            {
                bail_with_ctx!(e, "publish_presence_change");
            }

            info!("published user {}'s presence.", session.user_id);

            let presences = {
                let bulk = get_presences_bulk(&observable).await?;
                let mut presences = Vec::with_capacity(observable.len() + 1);
                presences.push(presence);
                for (user_id, (status, custom_status, devices, online_since)) in
                    observable.into_iter().zip(bulk)
                {
                    presences.push(Presence {
                        user_id,
                        status,
                        custom_status,
                        devices,
                        online_since,
                    });
                }

                presences
            };

            let (partial_guilds, dm_channels) = match session.prepare_ready_event(presences).await {
                Ok((ready, partial_guilds, dm_channels)) => {
                    if let Err(e) = tx.lock().await.send(session.encode(&ready)).await {
                        bail_with_ctx!(e, "send ready event: tx.send");
                    }
                    (partial_guilds, dm_channels)
                }
                Err(e) => {
                    bail_with_ctx!(e, "generate ready event: session.get_ready_event");
                }
            };

            // TODO: Resume, disable auto-delete for queues
            if let Err(e) = amqp
                .queue_declare(QueueDeclareArguments::transient_autodelete(
                    session.get_session_id_str(),
                ))
                .await
            {
                bail_with_ctx!(e, "declare queue: queue_declare");
            }

            let guild_ids: Vec<u64> = partial_guilds.iter().map(|g| g.id).collect();
            {
                let sid = session.get_session_id_str();
                for g in &guild_ids {
                    if let Err(e) = subscribe(&amqp, *g, sid, "topic").await {
                        bail_with_ctx!(e, "subscribe to guilds: subscribe");
                    }
                }
                for c in &dm_channels {
                    if let Err(e) = subscribe(&amqp, c.id, sid, "topic").await {
                        bail_with_ctx!(e, "subscribe to dm channels: subscribe");
                    }
                }
            }

            if let Err(e) = amqp
                .queue_bind(QueueBindArguments {
                    queue: session.get_session_id_str().to_string(),
                    exchange: "events".to_string(),
                    routing_key: format!("{}", session.user_id),
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
                    .manual_ack(false)
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
                    partial_guilds,
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
                let non_owned_guild_ids = guilds
                    .iter()
                    .filter(|g| g.partial.owner_id != session.user_id)
                    .map(|g| g.partial.id as i64)
                    .collect::<Vec<_>>();

                let mut members = if non_owned_guild_ids.is_empty() {
                    HashMap::new()
                } else {
                    get_pool()
                        .fetch_members_for_user_in_guilds(session.user_id, &non_owned_guild_ids)
                        .await
                        .map_err(|e| err_with_ctx!(e, "create hidden_channels: fetch_members_for_user_in_guilds"))?
                };

                for guild in guilds {
                    if guild.partial.owner_id == session.user_id {
                        continue;
                    }
                    let base_permissions = members
                        .remove(&guild.partial.id)
                        .ok_or("member not found while creating hidden_channels")?
                        .permissions;

                    if let Some(channels) = guild.channels {
                        if channels.is_empty() {
                            continue;
                        }

                        let mut roles = guild.roles.unwrap_or_default();
                        roles.sort_unstable_by_key(|r| r.position);

                        for channel in channels {
                            let perm = calculate_permissions_sorted(
                                session.user_id,
                                base_permissions,
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
                                ..
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
                                ..
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
                                            if let Err(e) = update_hidden_channels(
                                                guild.partial.id,
                                                session.user_id,
                                                chan.id,
                                                &mut hidden_channels,
                                                guild.roles.unwrap_or_default(),
                                                Some(&chan.overwrites)
                                            ).await {
                                                error!("failed to fetch member when updating permissions in ChannelCreate; guild: {} user: {} error: {e}", guild.partial.id, session.user_id);
                                                break;
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
                                after: EssenceChannel::Guild(chan),
                                ..
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
                                            if let Err(e) = update_hidden_channels(
                                                guild.partial.id,
                                                session.user_id,
                                                chan.id,
                                                &mut hidden_channels,
                                                guild.roles.unwrap_or_default(),
                                                Some(&chan.overwrites)
                                            ).await {
                                                error!("failed to fetch member when updating permissions in ChannelUpdate; guild: {} user: {} error: {e}", guild.partial.id, session.user_id);
                                                break;
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
                            OutboundMessage::ChannelDelete { channel_id, .. } => {
                                if let Err(e) =
                                    unsubscribe(&amqp, channel_id, session.get_session_id_str())
                                        .await
                                {
                                    error!("failed to unsubscribe to amqp exchange: {e:?}");
                                    break;
                                }
                            }
                            OutboundMessage::GuildCreate { guild, .. } => {
                                let guild_id = guild.partial.id;
                                if let Err(e) = subscribe(
                                    &amqp,
                                    guild_id,
                                    session.get_session_id_str(),
                                    "topic",
                                )
                                .await
                                {
                                    error!("failed to subscribe to amqp exchange: {e:?}");
                                    break;
                                }

                                // accomodate new member with presence update
                                let (status, custom_status) =
                                    match get_presence(session.user_id).await {
                                        Ok(p) => p,
                                        Err(e) => {
                                            error!("failed to get presence for member_join broadcast: {e:?}");
                                            continue;
                                        }
                                    };
                                let devices = match get_devices(session.user_id).await {
                                    Ok(d) => d,
                                    Err(e) => {
                                        error!("failed to get devices for member_join broadcast: {e:?}");
                                        continue;
                                    }
                                };
                                let online_since = match get_first_session(session.user_id).await {
                                    Ok(s) => s.map(|s| s.online_since),
                                    Err(e) => {
                                        error!("failed to get first session for member_join broadcast: {e:?}");
                                        continue;
                                    }
                                };
                                let presence = Presence {
                                    user_id: session.user_id,
                                    status,
                                    custom_status,
                                    devices,
                                    online_since,
                                };
                                if let Err(e) = publish_guild_event(
                                    &amqp,
                                    guild_id,
                                    OutboundMessage::PresenceUpdate { presence },
                                )
                                .await
                                {
                                    error!("failed to publish presence for member_join: {e:?}");
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
                                                roles.sort_unstable_by_key(|r| r.position);

                                                let Ok(Some(member)) = get_pool().fetch_member_by_id(guild.partial.id, session.user_id).await else {
                                                    error!("failed to fetch member when updating permissions in RoleUpdate; guild: {} user: {}", guild.partial.id, session.user_id);
                                                    break;
                                                };
                                                let base_permissions = member.permissions;

                                                for channel in channels {
                                                    let perm = calculate_permissions_sorted(
                                                        session.user_id,
                                                        base_permissions,
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
                while let Some(Ok(mut msg)) = rx.next().await {
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
                                status,
                                custom_status
                            } => {
                                if let Some(status) = &custom_status {
                                    if status.len() > 256 {
                                        if let Err(e) = tx
                                            .lock()
                                            .await
                                            .send(Message::Close(
                                                Some(CloseFrame {
                                                    code: CloseCode::Policy,
                                                    reason: "status length exceeds 256 characters".into() 
                                            }))).await {
                                                warn!("failed to send: {e:?}");
                                        }
                                        break;
                                    }
                                }

                                if let Err(e) = update_presence(session.user_id, status, custom_status.clone()).await {
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

                                let observable = match get_pool()
                                    .fetch_observable_user_ids_for_user(session.user_id)
                                    .await
                                {
                                    Ok(ids) => ids,
                                    Err(e) => {
                                        error!("failed to fetch observable users for presence update: {e:?}");
                                        break;
                                    }
                                };

                                if let Err(e) = publish_presence_change(
                                    &amqp,
                                    session.user_id,
                                    Presence {
                                        user_id: session.user_id,
                                        status,
                                        custom_status,
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
                                    &observable,
                                )
                                .await
                                {
                                    error!("error while publish presence change: {e:?}");
                                }
                            }
                            InboundMessage::RequestGuilds { guild_ids, nonce } => {
                                if guild_ids.len() > 20 {
                                    if let Err(e) = tx
                                        .lock()
                                        .await
                                        .send(Message::Close(Some(CloseFrame {
                                            code: CloseCode::Policy,
                                            reason: "at most 20 guild IDs may be requested at once".into(),
                                        })))
                                        .await
                                    {
                                        warn!("failed to send: {e:?}");
                                    }
                                    break;
                                }

                                let guilds = match get_pool()
                                    .fetch_guilds_by_ids(
                                        session.user_id,
                                        &guild_ids,
                                        GetGuildQuery {
                                            channels: true,
                                            roles: true,
                                            members: true,
                                            emojis: true,
                                        },
                                    )
                                    .await
                                {
                                    Ok(guilds) => guilds,
                                    Err(e) => {
                                        error!("failed to fetch guilds for RequestGuilds: {e:?}");
                                        break;
                                    }
                                };

                                let event = OutboundMessage::GuildsAvailable {
                                    guilds,
                                    nonce: nonce.clone(),
                                };
                                if let Err(e) = tx.lock().await.send(session.encode(&event)).await {
                                    debug!("failed to send GuildsAvailable to client: {e:?}");
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

            Ok(())
        }
        .await;

        let cleanup: Result<()> = {
            let r1 = remove_session(session.user_id, session.get_session_id_str()).await;
            let r2: Result<()> = async {
                if !any_session_exists(session.user_id).await? {
                    let observable = get_pool()
                        .fetch_observable_user_ids_for_user(session.user_id)
                        .await
                        .unwrap_or_default();
                    publish_presence_change(
                        &amqp,
                        session.user_id,
                        Presence {
                            user_id: session.user_id,
                            status: PresenceStatus::Offline,
                            custom_status: None,
                            devices: Devices::empty(),
                            online_since: None,
                        },
                        &observable,
                    )
                    .await?;
                    update_presence(session.user_id, PresenceStatus::Offline, None).await?;
                }
                Ok(())
            }
            .await;
            let r3 = amqp.close().await.map_err(Into::into);

            r1.and(r2).and(r3)
        };
        let cleanup_succeeded = cleanup.is_ok();

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
