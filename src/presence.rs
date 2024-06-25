use std::sync::OnceLock;

use amqprs::channel::Channel;
use bincode::{config::Configuration, Decode, Encode};
use chrono::{DateTime, Utc};
use deadpool_redis::{redis::{AsyncCommands, Pipeline}, Config, Connection, Pool, Runtime};
use essence::{
    db::{get_pool, UserDbExt},
    models::{Device, Devices, Presence, PresenceStatus},
    ws::OutboundMessage,
};
use futures_util::future::TryJoinAll;

use crate::{error::Result, events::publish_user_event};

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

    let _: () = pipe.query_async(&mut con).await?;

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

pub async fn update_presence(user_id: u64, status: PresenceStatus) -> Result<()> {
    let key = format!("presence-{user_id}");

    let mut con = get_con().await?;

    if status == PresenceStatus::Offline {
        con.del(key).await?;
    } else {
        con.set(key, bincode::encode_to_vec(status, CONFIG)?)
            .await?;
    }

    Ok(())
}

pub async fn get_presence(user_id: u64) -> Result<PresenceStatus> {
    let key = format!("presence-{user_id}");

    Ok(get_con()
        .await?
        .get::<_, Option<Vec<u8>>>(key)
        .await?
        .map_or_else(
            || PresenceStatus::Offline,
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
) -> Result<()> {
    let mut user_ids = get_pool()
        .fetch_observable_user_ids_for_user(user_id)
        .await?;
    user_ids.push(user_id);

    for user_id in user_ids {
        publish_user_event(
            channel,
            user_id,
            OutboundMessage::PresenceUpdate {
                presence: presence.clone(),
            },
        )
        .await?;
    }

    Ok(())
}
