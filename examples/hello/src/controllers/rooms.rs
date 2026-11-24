//! Chat rooms over WebSockets: `GET /rooms/{name}` upgrades, and each message goes to everyone in the room.

use doko::{
    Ctx, DurableObject, Result,
    axum::{
        Router,
        body::Body,
        extract::{Path, Request},
        http::Response,
        routing::get,
    },
};
use worker::{Env, WebSocket, WebSocketIncomingMessage, durable::State};

use crate::bindings::ROOMS;

pub fn routes() -> Router<Ctx> {
    Router::new().route("/rooms/{name}", get(join))
}

/// Hands the upgrade request to the room's object, which answers with the client's socket.
async fn join(ctx: Ctx, Path(name): Path<String>, req: Request) -> Result<Response<Body>> {
    ctx.durable(ROOMS)?.get(&name)?.fetch(req).await
}

/// One object per room. Its sockets hibernate, so a quiet room costs nothing.
pub struct Room(State);

impl DurableObject for Room {
    fn new(state: State, _: Env) -> Self {
        Self(state)
    }

    async fn fetch(&self, req: Request) -> Result<Response<Body>> {
        doko::accept_websocket(&self.0, &req)
    }

    async fn websocket_message(&self, _ws: WebSocket, message: WebSocketIncomingMessage) -> Result<()> {
        if let WebSocketIncomingMessage::String(text) = message {
            for peer in self.0.get_websockets() {
                peer.send_with_str(&text)?;
            }
        }
        Ok(())
    }
}
