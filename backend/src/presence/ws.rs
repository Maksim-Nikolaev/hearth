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
                    dispatch(&state, id, cm).await;
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

async fn dispatch(state: &AppState, from: uuid::Uuid, msg: crate::signaling::message::ClientMessage) {
    use crate::signaling::message::{ChatEntry, ClientMessage, ServerMessage};

    match msg {
        ClientMessage::Join { room } => {
            state.signaling.join_room(from, &room);

            // Deliver recent history to the joiner (oldest first).
            if let Ok(rows) = crate::chat::repository::recent(&state.pool, &room, 50).await {
                let messages: Vec<ChatEntry> = rows
                    .into_iter()
                    .rev()
                    .map(|r| ChatEntry {
                        from: r.from_user,
                        username: r.username,
                        body: r.body,
                        at: (r.created_at.unix_timestamp_nanos() / 1_000_000) as i64,
                    })
                    .collect();

                state.signaling.relay(from, ServerMessage::ChatHistory { messages });
            }
        }
        ClientMessage::Offer { to, flow, sdp } => state.signaling.relay(to, ServerMessage::Offer { from, flow, sdp }),
        ClientMessage::Answer { to, flow, sdp } => state.signaling.relay(to, ServerMessage::Answer { from, flow, sdp }),
        ClientMessage::Ice { to, flow, mline, candidate } => state.signaling.relay(to, ServerMessage::Ice { from, flow, mline, candidate }),
        ClientMessage::Chat { body } => {
            if let Some((username, room)) = state.signaling.user_context(from) {
                if let Ok(m) = crate::chat::repository::insert(&state.pool, &room, from, &body).await {
                    let at = (m.created_at.unix_timestamp_nanos() / 1_000_000) as i64;

                    state.signaling.broadcast(&room, ServerMessage::Chat { from, username, body, at });
                }
            }
        }
        ClientMessage::Leave => state.signaling.leave_room(from),
        ClientMessage::VoiceJoin => {}
        ClientMessage::VoiceLeave => {}
        ClientMessage::ShareStart => {}
        ClientMessage::ShareStop => {}
    }
}
