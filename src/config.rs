use std::{net::IpAddr, ops::Deref, sync::OnceLock};

use ahash::AHashSet;
use axum::extract::ws::Message;
use base64::{engine::general_purpose::STANDARD_NO_PAD, Engine};
use essence::{
    calculate_permissions,
    db::{get_pool, AuthDbExt, GuildDbExt, UserDbExt},
    http::guild::GetGuildQuery,
    models::{Guild, Permissions},
    ws::OutboundMessage,
};
use ring::rand::{SecureRandom, SystemRandom};
use serde::{Deserialize, Serialize};

use crate::error::{Error, IsNoneExt, Result};

static RNG: OnceLock<SystemRandom> = OnceLock::new();

#[inline]
fn get_rand() -> &'static SystemRandom {
    RNG.get_or_init(SystemRandom::new)
}

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
    pub hidden_channels: AHashSet<u64>,
}

impl UserSession {
    pub async fn new_with_token(
        con_config: Connection,
        token: String,
        ip: IpAddr,
    ) -> Result<(Self, Vec<Guild>)> {
        let db = get_pool();

        let (user_id, _flags) = db
            .fetch_user_info_by_token(&token)
            .await?
            .ok_or_close("Invalid token")?;

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

        Ok((
            Self {
                con_config,
                user_id,
                token,
                id: {
                    let mut s = String::with_capacity(40);

                    STANDARD_NO_PAD.encode_string(user_id.to_be_bytes(), &mut s);
                    s.push('.');

                    match ip {
                        IpAddr::V4(ip) => STANDARD_NO_PAD.encode_string(ip.octets(), &mut s),
                        IpAddr::V6(ip) => STANDARD_NO_PAD.encode_string(ip.octets(), &mut s),
                    };
                    s.push('.');

                    STANDARD_NO_PAD.encode_string(
                        {
                            let mut buf = Vec::with_capacity(40 - s.len());
                            get_rand().fill(&mut buf).expect("Failed to fill");

                            buf
                        },
                        &mut s,
                    );

                    STANDARD_NO_PAD.encode(s.as_bytes())
                },
                hidden_channels: {
                    let mut hidden = AHashSet::default();

                    for guild in &guilds {
                        if guild.partial.owner_id == user_id {
                            continue;
                        }

                        if let Some(channels) = &guild.channels {
                            let mut roles = guild.roles.clone().unwrap_or_default();

                            for channel in channels {
                                let perm = calculate_permissions(
                                    user_id,
                                    &mut roles,
                                    Some(&channel.overwrites),
                                );

                                if !perm.contains(Permissions::VIEW_CHANNEL) {
                                    hidden.insert(channel.id);
                                }
                            }
                        }
                    }

                    hidden
                },
            },
            guilds,
        ))
    }

    pub async fn get_ready_event(&self, guilds: Vec<Guild>) -> Result<OutboundMessage> {
        let db = get_pool();

        let user = db
            .fetch_client_user_by_id(self.user_id)
            .await?
            .ok_or_close("User does not exist")?;

        Ok(OutboundMessage::Ready {
            session_id: self.id.clone(),
            user,
            guilds: guilds,
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
