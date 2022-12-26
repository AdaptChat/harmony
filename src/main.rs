#[macro_use]
extern crate log;

mod config;
mod error;
mod websocket;

use axum::{
    extract::{Query, WebSocketUpgrade},
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
            |ws: WebSocketUpgrade, params: Query<config::ConnectionConfig>| async {
                ws.on_failed_upgrade(|e| warn!("Failed to upgrade: {:?}", e))
                    .on_upgrade(|socket| websocket::handle_socket(socket, params.0))
            },
        ),
    );

    axum::Server::bind(&"0.0.0.0:8076".parse().unwrap())
        .serve(app.into_make_service())
        .with_graceful_shutdown(async {
            tokio::signal::ctrl_c().await.ok();
        })
        .await
        .unwrap();
}
