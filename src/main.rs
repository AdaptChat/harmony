#[macro_use]
extern crate log;

mod config;
mod error;
mod websocket;

use std::{env, net::SocketAddr};

use axum::{
    extract::{ConnectInfo, Query, WebSocketUpgrade},
    http::HeaderMap,
    routing::get,
    Router,
};
use essence::db::connect;

#[tokio::main]
async fn main() {
    if std::env::var("RUST_LOG").is_err() {
        std::env::set_var("RUST_LOG", "info");
    }

    pretty_env_logger::init();

    drop(dotenv::dotenv());

    connect(&env::var("DATABASE_URL").expect("Missing DATABASE_URL env var"))
        .await
        .expect("Failed to connect to db");

    let app = Router::new()
        .route(
            "/",
            get(
                |ws: WebSocketUpgrade,
                 params: Query<config::Connection>,
                 headers: HeaderMap,
                 ConnectInfo(ip): ConnectInfo<SocketAddr>| async move {
                    ws.on_failed_upgrade(|e| warn!("Failed to upgrade: {e:?}"))
                        .on_upgrade(move |socket| async move {
                            drop(
                                websocket::handle_socket(
                                    socket,
                                    params.0,
                                    headers.get("cf-connecting-ip").map_or(ip.ip(), |v| {
                                        v.to_str()
                                            .unwrap_or_default()
                                            .parse()
                                            .unwrap_or_else(|_| ip.ip())
                                    }),
                                )
                                .await,
                            );
                        })
                },
            ),
        )
        .layer(tower_http::trace::TraceLayer::new_for_http());

    axum::Server::bind(&"0.0.0.0:8076".parse().unwrap())
        .serve(app.into_make_service_with_connect_info::<SocketAddr>())
        .with_graceful_shutdown(async {
            tokio::signal::ctrl_c().await.ok();
        })
        .await
        .unwrap();
}
