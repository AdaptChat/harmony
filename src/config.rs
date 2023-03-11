use std::{ops::Deref, str::FromStr, sync::OnceLock};

use ahash::RandomState;
use base64::{engine::general_purpose::STANDARD_NO_PAD, Engine};
use dashmap::DashSet;
use essence::{
    calculate_permissions_sorted,
    db::{get_pool, AuthDbExt, GuildDbExt, UserDbExt},
    http::guild::GetGuildQuery,
    models::{Guild, Permissions, Presence},
    ws::OutboundMessage,
};
use ring::rand::{SecureRandom, SystemRandom};
use serde::{Deserialize, Serialize};
use tokio_tungstenite::tungstenite::Message;

use crate::error::{Error, IsNoneExt, Result};

pub type HiddenChannels = DashSet<u64, RandomState>;

static RNG: OnceLock<SystemRandom> = OnceLock::new();

#[inline]
fn get_rand() -> &'static SystemRandom {
    RNG.get_or_init(SystemRandom::new)
}

#[derive(Clone, Copy, Debug, Default)]
pub enum MessageFormat {
    #[default]
    Json,
    Msgpack,
}

impl FromStr for MessageFormat {
    type Err = ();

    fn from_str(s: &str) -> std::result::Result<Self, Self::Err> {
        Ok(match s {
            "json" => Self::Json,
            "msgpack" => Self::Msgpack,
            _ => return Err(()),
        })
    }
}

#[derive(Debug, Clone, Default)]
pub struct Connection {
    pub version: u8,
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
    /// JSON must be sent with text while msgpack must be sent with binary
    #[allow(clippy::unused_self)]
    pub fn decode<'a, T: Deserialize<'a>>(&self, message: &'a mut Message) -> Result<T> {
        Ok(match message {
            // SAFETY: We are not using the string after passing to it, so it doesn't really matter if is valid UTF-8 or not.
            Message::Text(t) => unsafe { simd_json::from_str(t)? },
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
    pub async fn new_with_token(
        con_config: Connection,
        token: String,
    ) -> Result<(Self, Vec<Guild>, HiddenChannels)> {
        debug!("Creating user session");
        let db = get_pool();

        let (user_id, _flags) = db
            .fetch_user_info_by_token(&token)
            .await?
            .ok_or_close("Invalid token")?;
        debug!("Creating session for user {}...", user_id);
        
        let guilds = db
            .fetch_all_guilds_for_user(
                user_id,
                GetGuildQuery {
                    channels: true,
                    members: true,
                    roles: true,
                },
            )
            .await?;
        debug!("Fetched {} guilds for {}", guilds.len(), user_id);

        let hidden_channels = {
            let hidden = DashSet::default();

            for guild in &guilds {
                if guild.partial.owner_id == user_id {
                    continue;
                }

                if let Some(channels) = &guild.channels {
                    if channels.is_empty() {
                        continue;
                    }

                    let mut roles = guild.roles.clone().unwrap_or_default();
                    roles.sort_by_key(|r| r.position);

                    for channel in channels {
                        let perm = calculate_permissions_sorted(
                            user_id,
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

        Ok((
            Self {
                con_config,
                user_id,
                token,
                id: STANDARD_NO_PAD.encode({
                    let mut buf = [0; 16];
                    get_rand().fill(&mut buf).expect("Failed to fill");

                    buf
                }),
            },
            guilds,
            hidden_channels,
        ))
    }

    pub async fn get_ready_event(
        &self,
        guilds: Vec<Guild>,
        presences: Vec<Presence>,
    ) -> Result<OutboundMessage> {
        let db = get_pool();

        let user = db
            .fetch_client_user_by_id(self.user_id)
            .await?
            .ok_or_close("User does not exist")?;
        
        let relationships = db
            .fetch_relationships(self.user_id)
            .await?;
        
        let dm_channels = db
            .fetch_all_dm_channels_for_user(self.user_id)
            .await?;

        Ok(OutboundMessage::Ready {
            session_id: self.id.clone(),
            user,
            guilds,
            dm_channels,
            presences,
            relationships,
        })
    }
}

impl Deref for UserSession {
    type Target = Connection;

    fn deref(&self) -> &Self::Target {
        &self.con_config
    }
}
