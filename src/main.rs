#![feature(async_closure)]

#[macro_use]
extern crate log;

mod config;
mod error;
mod upstream;
mod websocket;

use std::{env, net::SocketAddr};

use axum::{
    extract::{ConnectInfo, Query, WebSocketUpgrade},
    http::HeaderMap,
    routing::get,
    Router,
};
use deadpool_lapin::Runtime;
use essence::db::connect;
use lapin::{options::ExchangeDeclareOptions, types::FieldTable, ExchangeKind};

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

    let pool = deadpool_lapin::Config {
        url: Some("amqp://127.0.0.1:5672".to_string()),
        ..Default::default()
    }
    .create_pool(Some(Runtime::Tokio1))
    .expect("Failed to create pool");

    pool.get()
        .await
        .expect("Failed to acquire connection")
        .create_channel()
        .await
        .expect("Failed to create channel")
        .exchange_declare(
            "events",
            ExchangeKind::Topic,
            ExchangeDeclareOptions::default(),
            FieldTable::default(),
        )
        .await
        .expect("Failed to create global event exchange");

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
                                    pool.get().await.expect("Failed to acquire connection"),
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
