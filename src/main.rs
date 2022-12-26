#[macro_use]
extern crate log;

mod config;
mod error;
mod websocket;

use std::net::SocketAddr;

use axum::{
    extract::{ConnectInfo, Query, WebSocketUpgrade},
    routing::get,
    Router,
};

#[tokio::main]
async fn main() {
    if std::env::var("RUST_LOG").is_err() {
        std::env::set_var("RUST_LOG", "info");
    }

    pretty_env_logger::init();

    let app = Router::new().route(
        "/",
        get(
            |ws: WebSocketUpgrade, params: Query<config::ConnectionConfig>, ConnectInfo(ip): ConnectInfo<SocketAddr>| async move {
                ws.on_failed_upgrade(|e| warn!("Failed to upgrade: {e:?}"))
                    .on_upgrade(move |socket| websocket::handle_socket(socket, params.0, ip))
            },
        ),
    );

    axum::Server::bind(&"0.0.0.0:8076".parse().unwrap())
        .serve(app.into_make_service_with_connect_info::<SocketAddr>())
        .with_graceful_shutdown(async {
            tokio::signal::ctrl_c().await.ok();
        })
        .await
        .unwrap();
}
