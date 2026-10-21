use std::collections::HashMap;
use std::sync::Arc;

use futures_util::{SinkExt, StreamExt};
use orb_core::admission::{AdmissionDecision, RoomAdmission};
use orb_core::network::is_lan_ip;
use orb_core::room::verify_password_hash;
use orb_core::types::{
    IceCandidatePayload, JoinRequestPayload, JoinResponsePayload, MsgType, SdpPayload,
    SignalingMessage,
};
use tokio::net::{TcpListener, TcpStream};
use tokio::sync::{broadcast, mpsc, watch, Mutex};
use tokio::task::AbortHandle;
use tokio_tungstenite::{accept_async, tungstenite::Message};
use tracing::{error, info, warn};

/// Events emitted by the signaling server to the Host application.
#[derive(Debug, Clone)]
pub enum SignalingEvent {
    /// A guest successfully joined the room.
    GuestJoined {
        guest_id: String,
        guest_name: String,
    },
    /// A guest left or was disconnected.
    GuestLeft { guest_id: String },
    /// An SDP offer received from a guest (forward to Host for WebRTC).
    SdpOffer { guest_id: String, sdp: String },
    /// An ICE candidate received from a guest.
    IceCandidate {
        guest_id: String,
        candidate: String,
        sdp_mid: Option<String>,
        sdp_mline_index: Option<u32>,
    },
}

struct GuestConnection {
    tx: mpsc::UnboundedSender<Message>,
}

/// WebSocket signaling server that Hosts run to accept Guest connections.
pub struct SignalingServer {
    addr: String,
    room_id: String,
    password_hash: String,
    password_salt: String,
    admission: RoomAdmission,
    guests: Arc<Mutex<HashMap<String, GuestConnection>>>,
    event_tx: broadcast::Sender<SignalingEvent>,
    next_guest_id: Arc<Mutex<u32>>,
    shutdown_tx: watch::Sender<bool>,
    accept_handle: Option<AbortHandle>,
}

impl SignalingServer {
    /// Create a new signaling server.
    ///
    /// `password_hash` should be SHA-256(password) provided by the room creator.
    /// `password_salt` is a random salt stored alongside the hash.
    ///
    /// Returns (server, event_rx) where event_rx receives SignalingEvent notifications.
    pub fn new(
        addr: String,
        room_id: String,
        password_hash: String,
        password_salt: String,
        admission: RoomAdmission,
    ) -> (Self, broadcast::Receiver<SignalingEvent>) {
        let (event_tx, event_rx) = broadcast::channel(256);
        let (shutdown_tx, _shutdown_rx) = watch::channel(false);
        let server = Self {
            addr,
            room_id,
            password_hash,
            password_salt,
            admission,
            guests: Arc::new(Mutex::new(HashMap::new())),
            event_tx,
            next_guest_id: Arc::new(Mutex::new(1)),
            shutdown_tx,
            accept_handle: None,
        };
        (server, event_rx)
    }

    /// Start listening for WebSocket connections. Spawns a background tokio task.
    pub async fn start(&mut self) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
        let listener = TcpListener::bind(&self.addr).await.map_err(|e| {
            if e.kind() == std::io::ErrorKind::AddrInUse {
                format!(
                    "端口 {} 被占用。当前共享服务未完全关闭，请稍后重试或更换端口。原始错误: {}",
                    self.addr, e
                )
            } else {
                format!("Failed to bind {}: {}", self.addr, e)
            }
        })?;
        info!("Signaling server listening on {}", self.addr);

        let guests = self.guests.clone();
        let event_tx = self.event_tx.clone();
        let room_id = self.room_id.clone();
        let password_hash = self.password_hash.clone();
        let password_salt = self.password_salt.clone();
        let admission = self.admission.clone();
        let next_guest_id = self.next_guest_id.clone();
        let mut shutdown_rx = self.shutdown_tx.subscribe();

        let handle = tokio::spawn(async move {
            loop {
                tokio::select! {
                    result = listener.accept() => {
                        match result {
                            Ok((stream, peer_addr)) => {
                                if !is_lan_ip(peer_addr.ip()) {
                                    warn!("Rejected non-LAN signaling connection from {}", peer_addr);
                                    drop(stream);
                                    continue;
                                }
                                info!("New TCP connection from {}", peer_addr);
                                let guests = guests.clone();
                                let event_tx = event_tx.clone();
                                let room_id = room_id.clone();
                                let password_hash = password_hash.clone();
                                let password_salt = password_salt.clone();
                                let next_guest_id = next_guest_id.clone();
                                let admission = admission.clone();

                                tokio::spawn(async move {
                                    if let Err(e) = handle_connection(
                                        stream,
                                        guests,
                                        event_tx,
                                        room_id,
                                        password_hash,
                                        password_salt,
                                        next_guest_id,
                                        admission,
                                    )
                                    .await
                                    {
                                        error!("Connection error: {}", e);
                                    }
                                });
                            }
                            Err(e) => {
                                error!("Accept error: {}", e);
                                break;
                            }
                        }
                    }
                    _ = shutdown_rx.changed() => {
                        if *shutdown_rx.borrow() {
                            info!("Signaling server shutting down (received shutdown signal)");
                            break;
                        }
                    }
                }
            }
            // Dropping listener here frees the port immediately
            drop(listener);
        });

        self.accept_handle = Some(handle.abort_handle());

        Ok(())
    }

    /// Gracefully shut down the signaling server.
    /// Aborts the accept loop (frees the port) and disconnects all guests.
    pub async fn shutdown(&mut self) {
        info!(
            "Signaling server shutdown initiated for room {}",
            self.room_id
        );

        // Signal the accept loop to stop
        let _ = self.shutdown_tx.send(true);

        // Abort the accept loop task if it's still running
        if let Some(handle) = self.accept_handle.take() {
            handle.abort();
        }

        // Disconnect all guests by closing their channels
        {
            let mut guests = self.guests.lock().await;
            info!("Disconnecting {} guests", guests.len());
            for guest_id in guests.keys() {
                self.admission.release(guest_id);
            }
            guests.clear();
        }

        info!(
            "Signaling server shutdown complete for room {}",
            self.room_id
        );
    }

    /// Send a message to a specific guest by their guest_id.
    pub async fn send_to_guest(
        &self,
        guest_id: &str,
        msg: SignalingMessage,
    ) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
        let guests = self.guests.lock().await;
        if let Some(guest) = guests.get(guest_id) {
            let json = serde_json::to_string(&msg)?;
            guest.tx.send(Message::Text(json.into()))?;
        } else {
            warn!("Guest {} not found", guest_id);
        }
        Ok(())
    }

    /// Broadcast a message to all connected guests.
    pub async fn broadcast(
        &self,
        msg: SignalingMessage,
    ) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
        let guests = self.guests.lock().await;
        let json = serde_json::to_string(&msg)?;
        for (id, guest) in guests.iter() {
            if let Err(e) = guest.tx.send(Message::Text(json.clone().into())) {
                warn!("Failed to send to guest {}: {}", id, e);
            }
        }
        Ok(())
    }
}

/// Verify the password hash from the client.
///
/// The client sends SHA-256(password). The server stores SHA-256(password + salt).
/// So we compute SHA-256(client_hash + salt) and compare with the stored hash.
///
/// If `stored_hash` is empty, no password is required — return true immediately.
/// Handle a single WebSocket connection from a client.
async fn handle_connection(
    stream: TcpStream,
    guests: Arc<Mutex<HashMap<String, GuestConnection>>>,
    event_tx: broadcast::Sender<SignalingEvent>,
    room_id: String,
    password_hash: String,
    password_salt: String,
    next_guest_id: Arc<Mutex<u32>>,
    admission: RoomAdmission,
) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    let ws_stream = accept_async(stream).await?;
    let (mut ws_tx, mut ws_rx) = ws_stream.split();

    let (tx, mut rx) = mpsc::unbounded_channel::<Message>();

    // Spawn a task to forward outgoing messages from the channel to the WebSocket.
    let write_task = tokio::spawn(async move {
        while let Some(msg) = rx.recv().await {
            if ws_tx.send(msg).await.is_err() {
                break;
            }
        }
    });

    let mut guest_id: Option<String> = None;

    // Process incoming messages.
    while let Some(result) = ws_rx.next().await {
        let msg = match result {
            Ok(m) => m,
            Err(e) => {
                warn!("WebSocket read error: {}", e);
                break;
            }
        };

        let text = match msg {
            Message::Text(t) => t.to_string(),
            Message::Close(_) => break,
            _ => continue,
        };

        let signaling_msg: SignalingMessage = match serde_json::from_str(&text) {
            Ok(m) => m,
            Err(e) => {
                warn!("Failed to parse signaling message: {}", e);
                continue;
            }
        };

        match signaling_msg.msg_type {
            MsgType::JoinRequest => {
                let payload: JoinRequestPayload =
                    match serde_json::from_value(signaling_msg.payload) {
                        Ok(p) => p,
                        Err(e) => {
                            warn!("Invalid JoinRequest payload: {}", e);
                            continue;
                        }
                    };

                // A duplicated join request on the same WebSocket is a retry,
                // not a second participant.
                if let Some(existing_guest_id) = guest_id.as_ref() {
                    let response = SignalingMessage {
                        msg_type: MsgType::JoinResponse,
                        room_id: room_id.clone(),
                        sender_id: "host".to_string(),
                        payload: serde_json::to_value(JoinResponsePayload {
                            success: true,
                            error: None,
                            guest_id: Some(existing_guest_id.clone()),
                        })?,
                        timestamp: now_timestamp(),
                    };
                    let _ = tx.send(Message::Text(serde_json::to_string(&response)?.into()));
                    continue;
                }

                // Verify password.
                let password_required = !password_hash.is_empty();
                let provided_password = !payload.password_hash.is_empty();
                let password_match =
                    verify_password_hash(&payload.password_hash, &password_hash, &password_salt);
                info!(
                    "[signaling] JoinRequest received: guest_name={:?}, room_password_required: {}, provided_password: {}, password_match: {}, stored_hash_empty: {}",
                    payload.guest_name,
                    password_required,
                    provided_password,
                    password_match,
                    password_hash.is_empty()
                );
                if password_match {
                    // Generate guest ID.
                    let mut next_id = next_guest_id.lock().await;
                    let new_guest_id = format!("guest-{}", *next_id);
                    *next_id += 1;
                    drop(next_id);

                    if admission.try_admit(&new_guest_id) == AdmissionDecision::Full {
                        let response = SignalingMessage {
                            msg_type: MsgType::JoinResponse,
                            room_id: room_id.clone(),
                            sender_id: "host".to_string(),
                            payload: serde_json::to_value(JoinResponsePayload {
                                success: false,
                                error: Some(format!(
                                    "会议人数已满（最多 {} 位观看者）",
                                    admission.max_guests()
                                )),
                                guest_id: None,
                            })?,
                            timestamp: now_timestamp(),
                        };
                        let _ = tx.send(Message::Text(serde_json::to_string(&response)?.into()));
                        warn!(
                            "Join rejected: room capacity reached ({}/{})",
                            admission.current_guests(),
                            admission.max_guests()
                        );
                        continue;
                    }

                    // Register guest.
                    {
                        let mut g = guests.lock().await;
                        g.insert(new_guest_id.clone(), GuestConnection { tx: tx.clone() });
                    }

                    // Send success response.
                    let response = SignalingMessage {
                        msg_type: MsgType::JoinResponse,
                        room_id: room_id.clone(),
                        sender_id: "host".to_string(),
                        payload: serde_json::to_value(JoinResponsePayload {
                            success: true,
                            error: None,
                            guest_id: Some(new_guest_id.clone()),
                        })?,
                        timestamp: now_timestamp(),
                    };
                    let _ = tx.send(Message::Text(serde_json::to_string(&response)?.into()));

                    // Emit event.
                    let _ = event_tx.send(SignalingEvent::GuestJoined {
                        guest_id: new_guest_id.clone(),
                        guest_name: payload.guest_name,
                    });

                    guest_id = Some(new_guest_id);
                    info!("Guest {:?} joined room {}", guest_id, room_id);
                } else {
                    // Password mismatch.
                    let response = SignalingMessage {
                        msg_type: MsgType::JoinResponse,
                        room_id: room_id.clone(),
                        sender_id: "host".to_string(),
                        payload: serde_json::to_value(JoinResponsePayload {
                            success: false,
                            error: Some("Invalid password".to_string()),
                            guest_id: None,
                        })?,
                        timestamp: now_timestamp(),
                    };
                    let _ = tx.send(Message::Text(serde_json::to_string(&response)?.into()));
                    warn!("Join rejected: invalid password");
                }
            }
            MsgType::SdpOffer => {
                if let Some(ref gid) = guest_id {
                    let payload: SdpPayload = match serde_json::from_value(signaling_msg.payload) {
                        Ok(p) => p,
                        Err(e) => {
                            warn!("Invalid SdpOffer payload: {}", e);
                            continue;
                        }
                    };
                    let _ = event_tx.send(SignalingEvent::SdpOffer {
                        guest_id: gid.clone(),
                        sdp: payload.sdp,
                    });
                }
            }
            MsgType::IceCandidate => {
                if let Some(ref gid) = guest_id {
                    let payload: IceCandidatePayload =
                        match serde_json::from_value(signaling_msg.payload) {
                            Ok(p) => p,
                            Err(e) => {
                                warn!("Invalid IceCandidate payload: {}", e);
                                continue;
                            }
                        };
                    let _ = event_tx.send(SignalingEvent::IceCandidate {
                        guest_id: gid.clone(),
                        candidate: payload.candidate,
                        sdp_mid: payload.sdp_mid,
                        sdp_mline_index: payload.sdp_mline_index,
                    });
                }
            }
            MsgType::Leave => {
                if let Some(ref gid) = guest_id {
                    info!("Guest {} left room {}", gid, room_id);
                    break;
                }
            }
            _ => {
                // Ignore other message types for now.
            }
        }
    }

    // Clean up: remove guest from the map and emit GuestLeft event.
    if let Some(ref gid) = guest_id {
        {
            let mut g = guests.lock().await;
            g.remove(gid);
        }
        let _ = event_tx.send(SignalingEvent::GuestLeft {
            guest_id: gid.clone(),
        });
        admission.release(gid);
        info!("Guest {} disconnected from room {}", gid, room_id);
    }

    write_task.abort();
    Ok(())
}

fn now_timestamp() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs()
}

#[cfg(test)]
mod tests {
    use super::*;
    use orb_core::admission::RoomAdmission;
    use orb_core::room::{hash_password_for_storage, hash_password_for_transport, RoomManager};
    use orb_signaling_client::SignalingClientExt;

    mod orb_signaling_client {
        use orb_core::types::{JoinResponsePayload, MsgType, SignalingMessage};
        use tokio_tungstenite::{connect_async, tungstenite::Message};

        pub struct TestClient {
            tx: futures_util::stream::SplitSink<
                tokio_tungstenite::WebSocketStream<
                    tokio_tungstenite::MaybeTlsStream<tokio::net::TcpStream>,
                >,
                Message,
            >,
            rx: futures_util::stream::SplitStream<
                tokio_tungstenite::WebSocketStream<
                    tokio_tungstenite::MaybeTlsStream<tokio::net::TcpStream>,
                >,
            >,
        }

        pub trait SignalingClientExt {
            async fn connect(addr: &str) -> Self;
            async fn join(&mut self, password_hash: String) -> JoinResponsePayload;
        }

        impl SignalingClientExt for TestClient {
            async fn connect(addr: &str) -> Self {
                let (stream, _) = connect_async(addr).await.expect("connect signaling server");
                let (tx, rx) = futures_util::StreamExt::split(stream);
                Self { tx, rx }
            }

            async fn join(&mut self, password_hash: String) -> JoinResponsePayload {
                let message = SignalingMessage {
                    msg_type: MsgType::JoinRequest,
                    room_id: "room".to_string(),
                    sender_id: String::new(),
                    payload: serde_json::json!({
                        "guest_name": "Test Guest",
                        "password_hash": password_hash,
                    }),
                    timestamp: super::now_timestamp(),
                };

                futures_util::SinkExt::send(
                    &mut self.tx,
                    Message::Text(serde_json::to_string(&message).unwrap().into()),
                )
                .await
                .expect("send join request");

                while let Some(result) = futures_util::StreamExt::next(&mut self.rx).await {
                    let message = result.expect("read websocket message");
                    if let Message::Text(text) = message {
                        let response: SignalingMessage =
                            serde_json::from_str(&text).expect("parse signaling response");
                        if response.msg_type == MsgType::JoinResponse {
                            return serde_json::from_value(response.payload)
                                .expect("parse join response payload");
                        }
                    }
                }

                panic!("server closed before join response");
            }
        }
    }

    fn free_loopback_addr() -> String {
        let listener = std::net::TcpListener::bind("127.0.0.1:0").expect("bind free port");
        let addr = listener.local_addr().expect("read local addr");
        drop(listener);
        addr.to_string()
    }

    #[tokio::test]
    async fn accepts_join_when_password_hash_matches() {
        let addr = free_loopback_addr();
        let mut room_manager = RoomManager::new();
        room_manager.password_salt = "test-salt".to_string();
        room_manager.set_password("secret");
        let password_hash = hash_password_for_transport("secret");
        let stored_hash = room_manager
            .password_hash
            .clone()
            .expect("room manager stores the password verifier");
        let (mut server, mut events) = SignalingServer::new(
            addr.clone(),
            "room".to_string(),
            stored_hash,
            room_manager.password_salt,
            RoomAdmission::new(10),
        );
        server.start().await.expect("start signaling server");

        let mut client = orb_signaling_client::TestClient::connect(&format!("ws://{}", addr)).await;
        let response = client.join(password_hash).await;

        assert!(response.success);
        assert_eq!(response.guest_id.as_deref(), Some("guest-1"));

        let event = tokio::time::timeout(std::time::Duration::from_secs(1), events.recv())
            .await
            .expect("guest joined event")
            .expect("receive event");
        assert!(matches!(event, SignalingEvent::GuestJoined { .. }));

        server.shutdown().await;
    }

    #[tokio::test]
    async fn rejects_join_when_password_hash_does_not_match() {
        let addr = free_loopback_addr();
        let salt = "test-salt".to_string();
        let stored_hash = hash_password_for_storage(&hash_password_for_transport("secret"), &salt);
        let (mut server, _events) = SignalingServer::new(
            addr.clone(),
            "room".to_string(),
            stored_hash,
            salt,
            RoomAdmission::new(10),
        );
        server.start().await.expect("start signaling server");

        let mut client = orb_signaling_client::TestClient::connect(&format!("ws://{}", addr)).await;
        let response = client.join(hash_password_for_transport("wrong")).await;

        assert!(!response.success);
        assert_eq!(response.error.as_deref(), Some("Invalid password"));
        assert!(response.guest_id.is_none());

        server.shutdown().await;
    }

    #[tokio::test]
    async fn rejects_join_when_shared_room_capacity_is_full() {
        let addr = free_loopback_addr();
        let admission = RoomAdmission::new(1);
        assert_eq!(
            admission.try_admit("browser-existing"),
            AdmissionDecision::Admitted
        );
        let (mut server, _events) = SignalingServer::new(
            addr.clone(),
            "room".to_string(),
            String::new(),
            String::new(),
            admission,
        );
        server.start().await.expect("start signaling server");

        let mut client = orb_signaling_client::TestClient::connect(&format!("ws://{}", addr)).await;
        let response = client.join(String::new()).await;

        assert!(!response.success);
        assert_eq!(
            response.error.as_deref(),
            Some("会议人数已满（最多 1 位观看者）")
        );
        assert!(response.guest_id.is_none());

        server.shutdown().await;
    }
}
