use std::ops::Deref;

use axum::extract::ws::Message;
use serde::{Deserialize, Serialize};

use crate::error::{Error, Result};

#[derive(Debug, Deserialize, Default)]
#[serde(rename_all = "lowercase")]
pub enum MessageFormat {
    #[default]
    Json,
    Msgpack,
}

#[inline(always)]
const fn default_version() -> u8 {
    1
}

#[derive(Debug, Deserialize)]
pub struct ConnectionConfig {
    #[serde(default = "default_version")]
    version: u8,
    #[serde(default)]
    format: MessageFormat,
}

impl ConnectionConfig {
    pub fn encode<T: Serialize>(&self, data: T) -> Message {
        match self.format {
            MessageFormat::Json => Message::Text(
                simd_json::to_string(&data)
                    .expect("Failed to serialize a model, a model should always be serializable"),
            ),
            MessageFormat::Msgpack => Message::Binary(
                rmp_serde::to_vec_named(&data)
                    .expect("Failed to serialize a model, a model should always be serializable"),
            ),
        }
    }

    pub fn decode<'a, T: Deserialize<'a>>(&self, message: &'a mut Message) -> Result<T>
    where
        Self: 'a,
    {
        Ok(match self.format {
            MessageFormat::Json => {
                match message {
                    // SAFETY: We are not using the string after passing to it, so it doesn't really matter if is valid UTF-8 or not.
                    Message::Text(ref mut t) => unsafe { simd_json::from_str(t)? },
                    Message::Binary(ref mut b) => simd_json::from_slice(b)?,
                    _ => return Err(Error::Ignore),
                }
            }
            MessageFormat::Msgpack => match message {
                Message::Text(_) => {
                    return Err(Error::InvalidFormat(
                        "Text is not allowed when chosen msgpack".to_string(),
                    ))
                }
                Message::Binary(b) => rmp_serde::from_slice(b)?,
                _ => return Err(Error::Ignore),
            },
        })
    }
}

pub struct UserSession {
    pub con_config: ConnectionConfig,
    pub token: String,
}

impl UserSession {
    pub fn new(con_config: ConnectionConfig, token: String) -> Self {
        Self { con_config, token }
    }
}

impl Deref for UserSession {
    type Target = ConnectionConfig;

    fn deref(&self) -> &Self::Target {
        &self.con_config
    }
}
