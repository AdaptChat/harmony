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
    let app = Router::new().route(
        "/",
        get(
            |ws: WebSocketUpgrade, params: Query<config::ConnectionConfig>| async {
                ws.on_upgrade(|socket| websocket::handle_socket(socket, params.0));
            },
        ),
    );

    axum::Server::bind(&"0.0.0.0:3000".parse().unwrap())
        .serve(app.into_make_service())
        .with_graceful_shutdown(async {
            tokio::signal::ctrl_c().await.ok();
        })
        .await
        .unwrap();
}
