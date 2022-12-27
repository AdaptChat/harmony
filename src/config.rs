use std::ops::Deref;

use axum::extract::ws::Message;
use rand::distributions::{Alphanumeric, DistString};
use serde::{Deserialize, Serialize};

use crate::error::{Error, Result};

#[derive(Debug, Deserialize, Default)]
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
#[derive(Debug, Deserialize)]
pub struct Connection {
    #[serde(default = "default_version")]
    version: u8,
    #[serde(default)]
    format: MessageFormat,
}

impl Connection {
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

pub struct UserSession {
    pub con_config: Connection,
    pub token: String,
    pub id: String,
}

impl UserSession {
    pub fn new(con_config: ConnectionConfig, token: String) -> Self {
        Self {
            con_config,
            token,
            id: Alphanumeric.sample_string(&mut rand::thread_rng(), 32),
        }
    }
}

impl Deref for UserSession {
    type Target = Connection;

    fn deref(&self) -> &Self::Target {
        &self.con_config
    }
}
