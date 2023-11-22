use std::{convert::Infallible, ops::Deref, str::FromStr};

use anyhow::{anyhow, Context};
use essence::{
    db::{get_pool, AuthDbExt, ChannelDbExt, GuildDbExt, UserDbExt},
    http::guild::GetGuildQuery,
    models::Presence,
    ws::OutboundMessage,
};
use serde::{Deserialize, Serialize};
use tokio_tungstenite::tungstenite::Message;
use uuid::Uuid;

pub const DEFAULT_VERSION: u8 = 0;

#[derive(Debug, Clone, Copy, Default)]
pub enum MessageFormat {
    #[default]
    Json,
    MsgPack,
}

impl FromStr for MessageFormat {
    type Err = Infallible;

    /// This method is intentionally infallible
    /// It will return default value when it can't parse.
    fn from_str(s: &str) -> Result<Self, Self::Err> {
        if s.eq_ignore_ascii_case("msgpack") {
            Ok(Self::MsgPack)
        } else {
            Ok(Self::default())
        }
    }
}

#[derive(Debug, Clone, Copy)]
pub struct ConnectionSettings {
    pub version: u8,
    pub format: MessageFormat,
}

impl ConnectionSettings {
    pub fn decode<'a, T: Deserialize<'a>>(&self, msg: &'a mut Message) -> anyhow::Result<T> {
        match msg {
            Message::Binary(b) => {
                Ok(rmp_serde::from_slice(b).context("rmp-serde failed to decode message")?)
            }
            Message::Text(t) => unsafe {
                Ok(simd_json::from_str(t).context("simd-json failed to decode message")?)
            },
            _ => Err(anyhow!("invalid message type while decoding")),
        }
    }

    pub fn encode<T: Serialize>(&self, data: &T) -> Message {
        match self.format {
            MessageFormat::Json => {
                Message::Text(simd_json::to_string(data).expect("simd-json failed to serialize"))
            }
            MessageFormat::MsgPack => Message::Binary(
                rmp_serde::to_vec_named(data).expect("rmp-serde failed to serialize"),
            ),
        }
    }
}

impl Default for ConnectionSettings {
    fn default() -> Self {
        Self {
            version: DEFAULT_VERSION,
            format: MessageFormat::default(),
        }
    }
}

#[derive(Debug, Clone)]
pub struct UserSession {
    pub settings: ConnectionSettings,
    pub session_id: Uuid,
    session_id_str: String,
    pub token: String,
    pub user_id: u64,
}

impl UserSession {
    pub async fn new(
        settings: ConnectionSettings,
        token: String,
    ) -> Result<Option<Self>, essence::Error> {
        let session_id = Uuid::new_v4();
        let info = get_pool().fetch_user_info_by_token(token.clone()).await?;

        if let Some((user_id, _)) = info {
            Ok(Some(Self {
                settings,
                session_id,
                session_id_str: session_id
                    .as_simple()
                    .encode_lower(&mut Uuid::encode_buffer())
                    .to_string(),
                token,
                user_id,
            }))
        } else {
            return Ok(None);
        }
    }

    pub fn get_session_id_str(&self) -> &str {
        &self.session_id_str
    }

    pub async fn get_ready_event(
        &self,
        presences: Vec<Presence>,
    ) -> Result<OutboundMessage, essence::Error> {
        let db = get_pool();

        let user = db.fetch_client_user_by_id(self.user_id).await?.ok_or(
            essence::Error::InvalidToken {
                message:
                    "user is deleted after connecting to ws and before ready event is generated"
                        .to_string(),
            },
        )?;
        let relationships = db.fetch_relationships(self.user_id).await?;
        let guilds = db
            .fetch_all_guilds_for_user(self.user_id, GetGuildQuery::all())
            .await?;
        let dm_channels = db.fetch_all_dm_channels_for_user(self.user_id).await?;

        Ok(OutboundMessage::Ready {
            session_id: self.session_id_str.to_string(),
            user,
            guilds,
            dm_channels,
            presences,
            relationships,
        })
    }
}

impl Deref for UserSession {
    type Target = ConnectionSettings;

    fn deref(&self) -> &Self::Target {
        &self.settings
    }
}
