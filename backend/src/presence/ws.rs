use crate::{security::jwt, state::AppState};
use axum::{
    extract::{ws::{Message, WebSocket, WebSocketUpgrade}, Query, State},
    response::{IntoResponse, Response},
};
use futures::{SinkExt, StreamExt};
use std::collections::HashMap;

pub async fn ws_handler(
    State(state): State<AppState>,
    Query(params): Query<HashMap<String, String>>,
    upgrade: WebSocketUpgrade,
) -> Response {
    let token = params.get("token").cloned().unwrap_or_default();

    match jwt::decode_access(&state.config.jwt_secret, &token) {
        Ok(claims) => upgrade.on_upgrade(move |socket| handle_socket(socket, state, claims.sub, claims.username)),
        Err(_) => axum::http::StatusCode::UNAUTHORIZED.into_response(),
    }
}

async fn handle_socket(socket: WebSocket, state: AppState, id: uuid::Uuid, username: String) {
    let mut rx = state.presence.subscribe();
    state.presence.mark_online(id, &username);

    let (mut sink, mut stream) = socket.split();

    // Forward presence events to this client until the socket errors.
    let forward = tokio::spawn(async move {
        while let Ok(event) = rx.recv().await {
            let json = serde_json::to_string(&event).unwrap();

            if sink.send(Message::Text(json)).await.is_err() {
                break;
            }
        }
    });

    // Drain inbound frames until the client disconnects.
    while let Some(Ok(msg)) = stream.next().await {
        if matches!(msg, Message::Close(_)) {
            break;
        }
    }

    forward.abort();
    state.presence.mark_offline(id, &username);
}
