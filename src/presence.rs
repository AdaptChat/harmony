use std::sync::OnceLock;

use bincode::{config::Configuration, Decode, Encode};
use chrono::{DateTime, Utc};
use deadpool_redis::{redis::AsyncCommands, Config, Connection, Pool, Runtime};

use crate::error::Result;

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
}

async fn get_sessions(con: &mut Connection, key: impl AsRef<str>) -> Result<Vec<PresenceSession>> {
    if let Some(sessions) = con.get::<_, Option<Vec<u8>>>(key.as_ref()).await? {
        Ok(bincode::decode_from_slice::<Vec<PresenceSession>, _>(
            &sessions,
            CONFIG,
        )?.0)
    }
    else {
        Ok(Vec::new())
    }
}

pub async fn get_last_session(user_id: u64) -> Result<Option<PresenceSession>> {
    let key = format!("pres-{user_id}");

    Ok(bincode::decode_from_slice::<Vec<PresenceSession>, _>(
        &get_con().await?.get::<_, Vec<u8>>(user_id).await?,
        CONFIG,
    )?
    .0
    .pop())
}

pub async fn insert_session(user_id: u64, session: PresenceSession) -> Result<()> {
    let mut con = get_con().await?;
    let key = format!("pres-{user_id}");

    let new_sessions =
        bincode::encode_to_vec(get_sessions(&mut con, &key).await?.push(session), CONFIG)?;

    con.set::<_, _, ()>(&key, new_sessions).await?;

    Ok(())
}

pub async fn remove_session(user_id: u64, session_id: impl AsRef<str>) -> Result<()> {
    let mut con = get_con().await?;
    let key = format!("pres-{user_id}");

    let mut sessions = get_sessions(&mut con, &key).await?;

    let index = sessions.iter().enumerate().fold(0, |acc, (i, v)| {
        if v.session_id == session_id.as_ref() {
            i
        } else {
            acc
        }
    });
    sessions.remove(index);

    let new_sessions = bincode::encode_to_vec(sessions, CONFIG)?;

    con.set::<_, _, ()>(&key, new_sessions).await?;

    Ok(())
}
