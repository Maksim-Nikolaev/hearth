use std::net::SocketAddr;

/// Bind to an ephemeral port, serve the app on a background task, return the address.
pub async fn spawn_app() -> SocketAddr {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();

    let router = hearth_backend::app::build_router();

    tokio::spawn(async move {
        axum::serve(listener, router).await.unwrap();
    });

    addr
}
