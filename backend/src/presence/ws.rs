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
    let mut presence_rx = state.presence.subscribe();
    let mut sig_rx = state.signaling.register(id, &username);
    state.presence.mark_online(id, &username);

    let (mut sink, mut stream) = socket.split();

    // Outbound: forward presence events and targeted signaling messages.
    let forward = tokio::spawn(async move {
        loop {
            tokio::select! {
                presence = presence_rx.recv() => match presence {
                    Ok(event) => {
                        let json = serde_json::to_string(&event).unwrap();
                        if sink.send(Message::Text(json)).await.is_err() { break; }
                    }
                    Err(_) => {} // lagged/closed broadcast: keep serving signaling
                },
                signal = sig_rx.recv() => match signal {
                    Some(msg) => {
                        let json = serde_json::to_string(&msg).unwrap();
                        if sink.send(Message::Text(json)).await.is_err() { break; }
                    }
                    None => break, // hub dropped this peer's sender
                },
            }
        }
    });

    // Inbound: parse client signaling messages and route them through the hub.
    while let Some(Ok(msg)) = stream.next().await {
        match msg {
            Message::Text(text) => {
                if let Ok(cm) = serde_json::from_str::<crate::signaling::message::ClientMessage>(&text) {
                    dispatch(&state, id, cm);
                }
            }
            Message::Close(_) => break,
            _ => {}
        }
    }

    forward.abort();
    state.signaling.disconnect(id);
    state.presence.mark_offline(id, &username);
}

fn dispatch(state: &AppState, from: uuid::Uuid, msg: crate::signaling::message::ClientMessage) {
    use crate::signaling::message::{ClientMessage, ServerMessage};

    match msg {
        ClientMessage::Join { room } => state.signaling.join_room(from, &room),
        ClientMessage::Offer { to, sdp } => state.signaling.relay(to, ServerMessage::Offer { from, sdp }),
        ClientMessage::Answer { to, sdp } => state.signaling.relay(to, ServerMessage::Answer { from, sdp }),
        ClientMessage::Ice { to, mline, candidate } => state.signaling.relay(to, ServerMessage::Ice { from, mline, candidate }),
        ClientMessage::Leave => state.signaling.leave_room(from),
    }
}
