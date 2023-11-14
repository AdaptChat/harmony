use std::net::IpAddr;

use anyhow::Context;
use qstring::QString;
use tokio::net::TcpStream;
use tokio_tungstenite::{WebSocketStream as _WebSocketStream, accept_hdr_async, tungstenite::handshake::server::Request};

use crate::config::{ConnectionSettings, DEFAULT_VERSION};

pub type WebSocketStream = _WebSocketStream<TcpStream>;

pub async fn accept(stream: TcpStream) -> anyhow::Result<(WebSocketStream, Option<IpAddr>, ConnectionSettings)> {
    let mut ip = None;
    let mut settings = ConnectionSettings::default();

    let websocket = accept_hdr_async(stream, |req: &Request, resp| {
        ip = req
            .headers()
            .get("cf-connecting-ip")
            .and_then(|v| v.to_str().ok())
            .and_then(|v| v.parse::<IpAddr>().ok());

        if let Some(query) = req.uri().query() {
            let queries = QString::from(query);

            let version = queries.get("version")
                .and_then(|v| v.parse::<u8>().ok())
                .unwrap_or(DEFAULT_VERSION);
            let format = queries.get("format")
                .and_then(|f| f.parse().ok())
                .unwrap_or_default();

            settings = ConnectionSettings { version, format };
        }

        Ok(resp)
    }).await.context("Failed to accept websocket stream")?;

    Ok((websocket, ip, settings))
}