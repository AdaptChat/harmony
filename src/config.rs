use std::{convert::Infallible, str::FromStr};

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
}

impl UserSession {
    pub fn new(settings: ConnectionSettings) -> Self {
        let session_id = Uuid::new_v4();

        Self {
            settings,
            session_id,
            session_id_str: session_id
                .as_simple()
                .encode_lower(&mut Uuid::encode_buffer())
                .to_string(),
        }
    }

    pub fn get_session_id_str(&self) -> &str {
        &self.session_id_str
    }
}
