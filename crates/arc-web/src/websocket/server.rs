use actix::prelude::*;
use std::collections::{HashMap, HashSet};
use std::sync::Arc;
use tracing::info;
use uuid::Uuid;

use crate::websocket::security::{
    OwnUserRoomPolicy, SharedWebSocketRoomPolicy, WebSocketPrincipal,
};

/// Message to connect a new client
#[derive(Message)]
#[rtype(result = "()")]
pub struct Connect {
    pub id: Uuid,
    pub user_id: Option<String>,
    pub addr: Recipient<WsMessage>,
}

/// Message to disconnect a client
#[derive(Message)]
#[rtype(result = "()")]
pub struct Disconnect {
    pub id: Uuid,
}

/// Message to subscribe to a room/channel
#[derive(Message)]
#[rtype(result = "()")]
pub struct Subscribe {
    pub id: Uuid,
    pub room: String,
}

/// Message to unsubscribe from a room/channel
#[derive(Message)]
#[rtype(result = "()")]
pub struct Unsubscribe {
    pub id: Uuid,
    pub room: String,
}

/// Message to broadcast to a specific room
#[derive(Message)]
#[rtype(result = "()")]
pub struct BroadcastToRoom {
    pub room: String,
    pub message: String,
    pub skip_id: Option<Uuid>,
}

/// Message to broadcast to a specific user
#[derive(Message)]
#[rtype(result = "()")]
pub struct BroadcastToUser {
    pub user_id: String,
    pub message: String,
}

/// Message to broadcast to all connected clients
#[derive(Message)]
#[rtype(result = "()")]
pub struct BroadcastAll {
    pub message: String,
    pub skip_id: Option<Uuid>,
}

/// WebSocket message to send to a client
#[derive(Message, Clone)]
#[rtype(result = "()")]
pub struct WsMessage(pub String);

/// Connection info stored in the server
struct ConnectionInfo {
    addr: Recipient<WsMessage>,
    user_id: Option<String>,
    rooms: HashSet<String>,
}

/// WebSocket broadcast server
/// Maintains registry of all active connections and handles message broadcasting
pub struct WsServer {
    /// Map of connection ID to connection info
    connections: HashMap<Uuid, ConnectionInfo>,
    /// Map of room name to set of connection IDs
    rooms: HashMap<String, HashSet<Uuid>>,
    /// Map of user ID to set of connection IDs (for user-specific broadcasts)
    user_connections: HashMap<String, HashSet<Uuid>>,
    room_policy: SharedWebSocketRoomPolicy,
}

impl WsServer {
    pub fn new() -> Self {
        Self::with_room_policy(Arc::new(OwnUserRoomPolicy))
    }

    pub fn with_room_policy(room_policy: SharedWebSocketRoomPolicy) -> Self {
        WsServer {
            connections: HashMap::new(),
            rooms: HashMap::new(),
            user_connections: HashMap::new(),
            room_policy,
        }
    }

    /// Send a message to a specific connection
    fn send_message(&self, id: &Uuid, message: &str) {
        if let Some(conn) = self.connections.get(id) {
            conn.addr.do_send(WsMessage(message.to_string()));
        }
    }

    /// Get the number of active connections
    pub fn connection_count(&self) -> usize {
        self.connections.len()
    }
}

impl Default for WsServer {
    fn default() -> Self {
        Self::new()
    }
}

impl Actor for WsServer {
    type Context = Context<Self>;
}

impl Handler<Connect> for WsServer {
    type Result = ();

    fn handle(&mut self, msg: Connect, _: &mut Context<Self>) {
        let user_id_for_log = msg.user_id.clone();
        let user_id_for_track = msg.user_id.clone();
        // Add connection to registry
        self.connections.insert(
            msg.id,
            ConnectionInfo {
                addr: msg.addr,
                user_id: msg.user_id,
                rooms: HashSet::new(),
            },
        );

        // Track user connections if authenticated
        if let Some(user_id) = user_id_for_track {
            self.user_connections
                .entry(user_id)
                .or_default()
                .insert(msg.id);
        }

        info!(connection_id = %msg.id, user_id = ?user_id_for_log, "WebSocket connection established");
    }
}

impl Handler<Disconnect> for WsServer {
    type Result = ();

    fn handle(&mut self, msg: Disconnect, _: &mut Context<Self>) {
        if let Some(conn) = self.connections.remove(&msg.id) {
            // Remove from all rooms
            for room in &conn.rooms {
                if let Some(room_members) = self.rooms.get_mut(room) {
                    room_members.remove(&msg.id);
                    if room_members.is_empty() {
                        self.rooms.remove(room);
                    }
                }
            }

            // Remove from user connections
            if let Some(user_id) = conn.user_id {
                if let Some(user_conns) = self.user_connections.get_mut(&user_id) {
                    user_conns.remove(&msg.id);
                    if user_conns.is_empty() {
                        self.user_connections.remove(&user_id);
                    }
                }
            }

            info!(connection_id = %msg.id, "WebSocket connection closed");
        }
    }
}

impl Handler<Subscribe> for WsServer {
    type Result = ();

    fn handle(&mut self, msg: Subscribe, _: &mut Context<Self>) {
        // Authorization happens in this actor, immediately before mutation,
        // so direct actor messages cannot bypass the WebSocket parser.
        let authorized = self
            .connections
            .get(&msg.id)
            .and_then(|conn| conn.user_id.as_deref())
            .and_then(WebSocketPrincipal::new)
            .is_some_and(|principal| self.room_policy.permits(&principal, &msg.room));
        if !authorized {
            return;
        }

        self.rooms
            .entry(msg.room.clone())
            .or_default()
            .insert(msg.id);
        if let Some(conn) = self.connections.get_mut(&msg.id) {
            conn.rooms.insert(msg.room);
        }
    }
}

impl Handler<Unsubscribe> for WsServer {
    type Result = ();

    fn handle(&mut self, msg: Unsubscribe, _: &mut Context<Self>) {
        // Remove connection from room
        if let Some(room_members) = self.rooms.get_mut(&msg.room) {
            room_members.remove(&msg.id);
            if room_members.is_empty() {
                self.rooms.remove(&msg.room);
            }
        }

        // Remove room from connection tracking
        if let Some(conn) = self.connections.get_mut(&msg.id) {
            conn.rooms.remove(&msg.room);
        }

        // Debug: Connection unsubscribed from room
    }
}

impl Handler<BroadcastToRoom> for WsServer {
    type Result = ();

    fn handle(&mut self, msg: BroadcastToRoom, _: &mut Context<Self>) {
        if let Some(members) = self.rooms.get(&msg.room) {
            for id in members {
                // Skip sender if specified
                if let Some(skip_id) = msg.skip_id {
                    if *id == skip_id {
                        continue;
                    }
                }
                self.send_message(id, &msg.message);
            }
        }
    }
}

impl Handler<BroadcastToUser> for WsServer {
    type Result = ();

    fn handle(&mut self, msg: BroadcastToUser, _: &mut Context<Self>) {
        if let Some(user_conns) = self.user_connections.get(&msg.user_id) {
            for id in user_conns {
                self.send_message(id, &msg.message);
            }
        }
    }
}

impl Handler<BroadcastAll> for WsServer {
    type Result = ();

    fn handle(&mut self, msg: BroadcastAll, _: &mut Context<Self>) {
        for id in self.connections.keys() {
            // Skip sender if specified
            if let Some(skip_id) = msg.skip_id {
                if *id == skip_id {
                    continue;
                }
            }
            self.send_message(id, &msg.message);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tokio::sync::mpsc;

    struct TestClient(mpsc::UnboundedSender<String>);

    impl Actor for TestClient {
        type Context = Context<Self>;
    }

    impl Handler<WsMessage> for TestClient {
        type Result = ();

        fn handle(&mut self, message: WsMessage, _: &mut Self::Context) {
            let _ = self.0.send(message.0);
        }
    }

    fn connect(
        server: &mut WsServer,
        id: Uuid,
        user_id: Option<&str>,
    ) -> mpsc::UnboundedReceiver<String> {
        let (tx, rx) = mpsc::unbounded_channel();
        let client = TestClient(tx).start();
        Handler::<Connect>::handle(
            server,
            Connect {
                id,
                user_id: user_id.map(str::to_owned),
                addr: client.recipient(),
            },
            &mut Context::new(),
        );
        // The recipient stored by WsServer keeps delivery addressed to this actor.
        rx
    }

    #[actix::test]
    async fn actor_authorizes_membership_and_preserves_user_delivery() {
        let mut server = WsServer::new();
        let alice_id = Uuid::new_v4();
        let bob_id = Uuid::new_v4();
        let mut alice_messages = connect(&mut server, alice_id, Some("alice"));
        let mut bob_messages = connect(&mut server, bob_id, Some("bob"));

        Handler::<Subscribe>::handle(
            &mut server,
            Subscribe {
                id: alice_id,
                room: "user:alice".into(),
            },
            &mut Context::new(),
        );
        Handler::<Subscribe>::handle(
            &mut server,
            Subscribe {
                id: alice_id,
                room: "user:bob".into(),
            },
            &mut Context::new(),
        );
        Handler::<Subscribe>::handle(
            &mut server,
            Subscribe {
                id: alice_id,
                room: "unknown".into(),
            },
            &mut Context::new(),
        );
        Handler::<Subscribe>::handle(
            &mut server,
            Subscribe {
                id: bob_id,
                room: "user:alice".into(),
            },
            &mut Context::new(),
        );

        assert_eq!(
            server.rooms.get("user:alice"),
            Some(&HashSet::from([alice_id]))
        );
        assert!(!server.rooms.contains_key("user:bob"));
        assert!(!server.rooms.contains_key("unknown"));

        Handler::<BroadcastToRoom>::handle(
            &mut server,
            BroadcastToRoom {
                room: "user:alice".into(),
                message: "room-message".into(),
                skip_id: None,
            },
            &mut Context::new(),
        );
        Handler::<BroadcastToUser>::handle(
            &mut server,
            BroadcastToUser {
                user_id: "alice".into(),
                message: "user-message".into(),
            },
            &mut Context::new(),
        );
        assert_eq!(alice_messages.recv().await.as_deref(), Some("room-message"));
        assert_eq!(alice_messages.recv().await.as_deref(), Some("user-message"));
        assert!(
            tokio::time::timeout(std::time::Duration::from_millis(25), bob_messages.recv())
                .await
                .is_err()
        );

        Handler::<Unsubscribe>::handle(
            &mut server,
            Unsubscribe {
                id: alice_id,
                room: "user:alice".into(),
            },
            &mut Context::new(),
        );
        assert!(!server.rooms.contains_key("user:alice"));
        assert!(!server
            .connections
            .get(&alice_id)
            .unwrap()
            .rooms
            .contains("user:alice"));

        Handler::<Subscribe>::handle(
            &mut server,
            Subscribe {
                id: alice_id,
                room: "user:alice".into(),
            },
            &mut Context::new(),
        );
        Handler::<Disconnect>::handle(
            &mut server,
            Disconnect { id: alice_id },
            &mut Context::new(),
        );
        assert!(!server.rooms.contains_key("user:alice"));
        assert!(!server.connections.contains_key(&alice_id));
        assert!(!server.user_connections.contains_key("alice"));
    }

    #[test]
    fn unknown_connection_id_cannot_create_room_membership() {
        let mut server = WsServer::new();
        Handler::<Subscribe>::handle(
            &mut server,
            Subscribe {
                id: Uuid::new_v4(),
                room: "user:alice".into(),
            },
            &mut Context::new(),
        );
        assert!(server.rooms.is_empty());
    }
}
