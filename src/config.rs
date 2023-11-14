use std::{str::FromStr, convert::Infallible};

pub const DEFAULT_VERSION: u8 = 0;

#[derive(Debug, Clone, Copy, Default)]
pub enum MessageFormat {
    #[default]
    Json,
    MsgPack
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
    pub format: MessageFormat
}

impl Default for ConnectionSettings {
    fn default() -> Self {
        Self {
            version: DEFAULT_VERSION,
            format: MessageFormat::default()
        }
    }
}