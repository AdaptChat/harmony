use std::ops::Deref;

use axum::extract::ws::Message;
use essence::{
    db::{get_pool, AuthDbExt, GuildDbExt, UserDbExt},
    http::guild::GetGuildQuery,
    models::Guild,
    ws::OutboundMessage,
};
use rand::distributions::{Alphanumeric, DistString};
use serde::{Deserialize, Serialize};

use crate::error::{Error, IsNoneExt, Result};

#[derive(Clone, Copy, Debug, Deserialize, Default)]
#[serde(rename_all = "lowercase")]
pub enum MessageFormat {
    #[default]
    Json,
    Msgpack,
}

#[inline]
const fn default_version() -> u8 {
    1
}

#[allow(clippy::unsafe_derive_deserialize)]
#[derive(Debug, Deserialize, Clone)]
pub struct Connection {
    #[serde(default = "default_version")]
    pub version: u8,
    #[serde(default)]
    pub format: MessageFormat,
}

impl Connection {
    pub fn encode<T: Serialize>(&self, data: &T) -> Message {
        match self.format {
            MessageFormat::Json => Message::Text(
                simd_json::to_string(data)
                    .expect("Failed to serialize a model, a model should always be serializable"),
            ),
            MessageFormat::Msgpack => Message::Binary(
                rmp_serde::to_vec_named(data)
                    .expect("Failed to serialize a model, a model should always be serializable"),
            ),
        }
    }

    /// Decode a message
    ///
    /// JSON must be sent with text while msgpack must be sent with
    #[allow(clippy::unused_self)]
    pub fn decode<'a, T: Deserialize<'a>>(&self, message: &'a mut Message) -> Result<T>
    where
        Self: 'a,
    {
        Ok(match message {
            // SAFETY: We are not using the string after passing to it, so it doesn't really matter if is valid UTF-8 or not.
            Message::Text(ref mut t) => unsafe { simd_json::from_str(t)? },
            Message::Binary(b) => rmp_serde::from_slice(b)?,
            _ => return Err(Error::Ignore),
        })
    }
}

#[derive(Debug, Clone)]
pub struct UserSession {
    pub con_config: Connection,
    pub token: String,
    pub id: String,
    pub user_id: u64,
}

impl UserSession {
    pub async fn new_with_token(con_config: Connection, token: String) -> Result<Self> {
        let (user_id, _flags) = get_pool()
            .fetch_user_info_by_token(&token)
            .await?
            .ok_or_close("Invalid token".to_string())?;

        Ok(Self {
            con_config,
            user_id,
            token,
            id: Alphanumeric.sample_string(&mut rand::thread_rng(), 32),
        })
    }

    pub async fn get_ready_event(&self) -> Result<OutboundMessage> {
        let db = get_pool();

        let (user, guilds) = tokio::join!(
            db.fetch_client_user_by_id(self.user_id),
            db.fetch_all_guilds_for_user(
                self.user_id,
                GetGuildQuery {
                    channels: true,
                    members: true,
                    roles: true
                }
            )
        );
        let user = user?.ok_or_close("User does not exist")?;

        Ok(OutboundMessage::Ready {
            session_id: self.id.clone(),
            user,
            guilds: guilds?,
        })
    }

    pub async fn get_guilds(&self) -> Result<Vec<Guild>> {
        Ok(get_pool()
            .fetch_all_guilds_for_user(
                self.user_id,
                GetGuildQuery {
                    channels: true,
                    members: true,
                    roles: true,
                },
            )
            .await?)
    }
}

impl Deref for UserSession {
    type Target = Connection;

    fn deref(&self) -> &Self::Target {
        &self.con_config
    }
}
