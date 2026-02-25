use std::sync::OnceLock;

use crate::{error::Result, events::publish_user_event};
use amqprs::channel::Channel;
use bincode::{config::Configuration, Decode, Encode};
use chrono::{DateTime, Utc};
use deadpool_redis::{
    redis::{AsyncCommands, Pipeline},
    Config, Connection, Pool, Runtime,
};
use essence::{
    models::{Device, Devices, Presence, PresenceStatus},
    ws::OutboundMessage,
};

static POOL: OnceLock<Pool> = OnceLock::new();
const CONFIG: Configuration = bincode::config::standard();

async fn get_con() -> Result<Connection> {
    Ok(POOL
        .get_or_init(|| {
            Config::from_url("redis://127.0.0.1")
                .create_pool(Some(Runtime::Tokio1))
                .unwrap()
        })
        .get()
        .await?)
}

#[derive(Debug, Encode, Decode, Clone)]
pub struct PresenceSession {
    pub session_id: String,
    #[bincode(with_serde)]
    pub online_since: DateTime<Utc>,
    pub device: Device,
}

pub async fn reset_all() -> Result<()> {
    let mut con = get_con().await?;

    let session_keys = con.keys::<_, Vec<String>>("session-*").await?;
    let presence_keys = con.keys::<_, Vec<String>>("presence-*").await?;

    let mut pipe = Pipeline::with_capacity(session_keys.len() + presence_keys.len());

    for key in session_keys.into_iter().chain(presence_keys.into_iter()) {
        pipe.del(key).ignore();
    }

    if !pipe.is_empty() {
        let _: () = pipe.query_async(&mut con).await?;
    }
    Ok(())
}

async fn get_sessions(con: &mut Connection, key: impl AsRef<str>) -> Result<Vec<PresenceSession>> {
    if let Some(sessions) = con
        .lrange::<_, Option<Vec<Vec<u8>>>>(key.as_ref(), 0, -1)
        .await?
    {
        if sessions.is_empty() {
            return Ok(Vec::new());
        }

        let mut res = Vec::with_capacity(sessions.len());

        for session in sessions.into_iter() {
            res.push(bincode::decode_from_slice(&session, CONFIG)?.0)
        }

        Ok(res)
    } else {
        Ok(Vec::new())
    }
}

pub async fn get_devices(user_id: u64) -> Result<Devices> {
    let mut devices = Devices::empty();

    for session in get_sessions(&mut get_con().await?, &format!("session-{user_id}")).await? {
        match session.device {
            Device::Desktop => devices.insert(Devices::DESKTOP),
            Device::Mobile => devices.insert(Devices::MOBILE),
            Device::Web => devices.insert(Devices::WEB),
        }

        if devices.is_all() {
            break;
        }
    }

    Ok(devices)
}

pub async fn get_first_session(user_id: u64) -> Result<Option<PresenceSession>> {
    let key = format!("session-{user_id}");

    if let Some(session) = get_con()
        .await?
        .lindex::<_, Option<Vec<u8>>>(key, 0)
        .await?
    {
        Ok(Some(bincode::decode_from_slice(&session, CONFIG)?.0))
    } else {
        Ok(None)
    }
}

pub async fn insert_session(user_id: u64, session: PresenceSession) -> Result<()> {
    let key = format!("session-{user_id}");

    get_con()
        .await?
        .rpush::<_, _, ()>(key, bincode::encode_to_vec(session, CONFIG)?)
        .await?;

    Ok(())
}

pub async fn remove_session(user_id: u64, session_id: impl AsRef<str>) -> Result<()> {
    let mut con = get_con().await?;
    let key = format!("session-{user_id}");

    let sessions = get_sessions(&mut con, &key).await?;

    if sessions.len() == 1 {
        con.del::<_, ()>(key).await?;

        return Ok(());
    }

    let index = sessions.iter().enumerate().fold(0, |acc, (i, v)| {
        if v.session_id == session_id.as_ref() {
            i
        } else {
            acc
        }
    });

    con.lset(&key, index as isize, "REMOVED").await?;
    con.lrem(key, 1, "REMOVED").await?;

    Ok(())
}

pub async fn any_session_exists(user_id: u64) -> Result<bool> {
    Ok(get_con()
        .await?
        .llen::<_, u16>(format!("session-{user_id}"))
        .await?
        > 0)
}

pub async fn update_presence(
    user_id: u64,
    status: PresenceStatus,
    custom_status: Option<String>,
) -> Result<()> {
    let key = format!("presence-{user_id}");

    let mut con = get_con().await?;

    if status == PresenceStatus::Offline {
        con.del(key).await?;
    } else {
        con.set(
            key,
            bincode::encode_to_vec((status, custom_status), CONFIG)?,
        )
        .await?;
    }

    Ok(())
}

/// Fetches presence data for the users with the provided IDs.
///
/// Returns a `Vec` parallel to `user_ids`, with each entry being
/// `(status, custom_status, devices, online_since)`.
pub async fn get_presences_bulk(
    user_ids: &[u64],
) -> Result<
    Vec<(
        PresenceStatus,
        Option<String>,
        Devices,
        Option<chrono::DateTime<Utc>>,
    )>,
> {
    if user_ids.is_empty() {
        return Ok(Vec::new());
    }

    let mut con = get_con().await?;

    let mut pipe1 = Pipeline::with_capacity(user_ids.len() * 2);
    for &uid in user_ids {
        pipe1.get(format!("presence-{uid}"));
        pipe1.lindex(format!("session-{uid}"), 0);
    }
    let scalars: Vec<Option<Vec<u8>>> = pipe1.query_async(&mut con).await?;

    let mut pipe2 = Pipeline::with_capacity(user_ids.len());
    for &uid in user_ids {
        pipe2.lrange(format!("session-{uid}"), 0, -1);
    }
    let lists: Vec<Option<Vec<Vec<u8>>>> = pipe2.query_async(&mut con).await?;

    let mut result = Vec::with_capacity(user_ids.len());
    for (i, sessions_raw) in lists.into_iter().enumerate() {
        let base = i * 2;

        let (status, custom_status) = scalars[base].as_deref().map_or_else(
            || (PresenceStatus::Offline, None),
            |bytes| {
                bincode::decode_from_slice(bytes, CONFIG)
                    .expect("malformed presence value in Redis")
                    .0
            },
        );

        let online_since: Option<chrono::DateTime<Utc>> =
            scalars[base + 1].as_deref().map(|bytes| {
                bincode::decode_from_slice::<PresenceSession, _>(bytes, CONFIG)
                    .expect("malformed session value in Redis")
                    .0
                    .online_since
            });

        let mut devices = Devices::empty();
        if let Some(sessions) = sessions_raw {
            for bytes in sessions {
                if let Ok((s, _)) = bincode::decode_from_slice::<PresenceSession, _>(&bytes, CONFIG)
                {
                    match s.device {
                        Device::Desktop => devices.insert(Devices::DESKTOP),
                        Device::Mobile => devices.insert(Devices::MOBILE),
                        Device::Web => devices.insert(Devices::WEB),
                    }
                    if devices.is_all() {
                        break;
                    }
                }
            }
        }

        result.push((status, custom_status, devices, online_since));
    }

    Ok(result)
}

pub async fn get_presence(user_id: u64) -> Result<(PresenceStatus, Option<String>)> {
    let key = format!("presence-{user_id}");

    Ok(get_con()
        .await?
        .get::<_, Option<Vec<u8>>>(key)
        .await?
        .map_or_else(
            || (PresenceStatus::Offline, Default::default()),
            |r| {
                bincode::decode_from_slice(&r, CONFIG)
                    .expect("Malformed value in key: {key}")
                    .0
            },
        ))
}

pub async fn publish_presence_change(
    channel: &Channel,
    user_id: u64,
    presence: Presence,
    observable_user_ids: &[u64],
) -> Result<()> {
    for &uid in observable_user_ids.iter().chain([&user_id]) {
        publish_user_event(
            channel,
            uid,
            OutboundMessage::PresenceUpdate {
                presence: presence.clone(),
            },
        )
        .await?;
    }

    Ok(())
}
