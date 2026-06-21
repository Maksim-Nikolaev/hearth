use hearth_backend::app;

#[tokio::main]
async fn main() {
    let listener = tokio::net::TcpListener::bind("0.0.0.0:8080").await.unwrap();

    println!("hearth-backend listening on {}", listener.local_addr().unwrap());

    axum::serve(listener, app::build_router()).await.unwrap();
}
