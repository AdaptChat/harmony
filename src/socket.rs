use std::net::IpAddr;

use qstring::QString;
use tokio::net::TcpStream;
use tokio_tungstenite::{
    accept_hdr_async, tungstenite::handshake::server::Request, WebSocketStream,
};

use crate::{
    config::{Connection, MessageFormat},
    error::Result,
};

pub async fn accept(
    stream: TcpStream,
) -> Result<(WebSocketStream<TcpStream>, Connection, Option<IpAddr>)> {
    let mut con = Connection::default();
    let mut ip = None;

    let websocket = accept_hdr_async(stream, |req: &Request, resp| {
        let ip_ = req
            .headers()
            .get("cf-connecting-ip")
            .map(|ip| ip.to_str().map(|ip| ip.parse::<IpAddr>().ok()).ok())
            .flatten()
            .flatten();

        let con_ = if let Some(query) = req.uri().query() {
            let query = QString::from(query);

            let format = if let Some(format) = query.get("format") {
                match format {
                    "msgpack" => MessageFormat::Msgpack,
                    _ => MessageFormat::default(),
                }
            } else {
                MessageFormat::default()
            };

            let version = query
                .get("version")
                .map(|v| v.parse::<u8>().ok())
                .flatten()
                .unwrap_or(0);

            Connection { version, format }
        } else {
            Connection::default()
        };

        con = con_;
        ip = ip_;

        Ok(resp)
    })
    .await?;

    Ok((websocket, con, ip))
}
