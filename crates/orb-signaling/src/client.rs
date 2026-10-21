use futures_util::{SinkExt, StreamExt};
use orb_core::types::{
    IceCandidatePayload, JoinRequestPayload, MsgType, SdpPayload, SignalingMessage,
};
use tokio::sync::mpsc;
use tokio_tungstenite::{connect_async, tungstenite::Message};
use tracing::{info, warn};

/// Events received by the signaling client from the server.
#[derive(Debug, Clone)]
pub enum SignalingEvent {
    /// Response to a join request.
    JoinResult {
        success: bool,
        error: Option<String>,
        guest_id: Option<String>,
    },
    /// An SDP answer from the Host (in response to our offer).
    SdpAnswer { sdp: String },
    /// An ICE candidate from the Host.
    IceCandidate {
        candidate: String,
        sdp_mid: Option<String>,
        sdp_mline_index: Option<u32>,
    },
    /// The room was closed by the Host.
    RoomClosed,
    /// JPEG fallback frame relayed by the signaling server.
    VideoFrame { data_url: String },
}

/// WebSocket signaling client used by Guests to connect to a Host's signaling server.
pub struct SignalingClient {
    ws_tx: mpsc::UnboundedSender<Message>,
    event_rx: mpsc::UnboundedReceiver<SignalingEvent>,
}

impl SignalingClient {
    /// Connect to a signaling server at the given WebSocket address.
    ///
    /// `addr` should be a WebSocket URL, e.g. "ws://192.168.1.10:9000".
    pub async fn connect(addr: &str) -> Result<Self, Box<dyn std::error::Error + Send + Sync>> {
        let (ws_stream, _) = connect_async(addr).await?;
        info!("Connected to signaling server at {}", addr);

        let (mut ws_tx_split, mut ws_rx_split) = ws_stream.split();
        let (outgoing_tx, mut outgoing_rx) = mpsc::unbounded_channel::<Message>();
        let (event_tx, event_rx) = mpsc::unbounded_channel::<SignalingEvent>();

        // Spawn task: forward outgoing messages from channel to WebSocket.
        tokio::spawn(async move {
            while let Some(msg) = outgoing_rx.recv().await {
                if ws_tx_split.send(msg).await.is_err() {
                    break;
                }
            }
        });

        // Spawn task: read incoming WebSocket messages and convert to SignalingEvent.
        tokio::spawn(async move {
            while let Some(result) = ws_rx_split.next().await {
                let msg = match result {
                    Ok(m) => m,
                    Err(e) => {
                        warn!("WebSocket read error: {}", e);
                        break;
                    }
                };

                let text = match msg {
                    Message::Text(t) => t.to_string(),
                    Message::Close(_) => {
                        let _ = event_tx.send(SignalingEvent::RoomClosed);
                        break;
                    }
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
                    MsgType::JoinResponse => {
                        let payload: orb_core::types::JoinResponsePayload =
                            match serde_json::from_value(signaling_msg.payload) {
                                Ok(p) => p,
                                Err(e) => {
                                    warn!("Invalid JoinResponse payload: {}", e);
                                    continue;
                                }
                            };
                        let _ = event_tx.send(SignalingEvent::JoinResult {
                            success: payload.success,
                            error: payload.error,
                            guest_id: payload.guest_id,
                        });
                    }
                    MsgType::SdpAnswer => {
                        let payload: SdpPayload =
                            match serde_json::from_value(signaling_msg.payload) {
                                Ok(p) => p,
                                Err(e) => {
                                    warn!("Invalid SdpAnswer payload: {}", e);
                                    continue;
                                }
                            };
                        let _ = event_tx.send(SignalingEvent::SdpAnswer { sdp: payload.sdp });
                    }
                    MsgType::IceCandidate => {
                        let payload: IceCandidatePayload =
                            match serde_json::from_value(signaling_msg.payload) {
                                Ok(p) => p,
                                Err(e) => {
                                    warn!("Invalid IceCandidate payload: {}", e);
                                    continue;
                                }
                            };
                        let _ = event_tx.send(SignalingEvent::IceCandidate {
                            candidate: payload.candidate,
                            sdp_mid: payload.sdp_mid,
                            sdp_mline_index: payload.sdp_mline_index,
                        });
                    }
                    MsgType::RoomClosed => {
                        let _ = event_tx.send(SignalingEvent::RoomClosed);
                        break;
                    }
                    MsgType::VideoFrame => {
                        if let Some(data_url) = signaling_msg
                            .payload
                            .get("dataUrl")
                            .and_then(|value| value.as_str())
                        {
                            let _ = event_tx.send(SignalingEvent::VideoFrame {
                                data_url: data_url.to_string(),
                            });
                        }
                    }
                    _ => {
                        // Ignore other message types.
                    }
                }
            }
        });

        Ok(Self {
            ws_tx: outgoing_tx,
            event_rx,
        })
    }

    /// Send a join request to the signaling server.
    ///
    /// `password_hash` should be SHA-256(password) computed by the client.
    pub fn join(
        &self,
        room_id: &str,
        guest_name: &str,
        password_hash: &str,
    ) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
        let msg = SignalingMessage {
            msg_type: MsgType::JoinRequest,
            room_id: room_id.to_string(),
            sender_id: String::new(), // Will be assigned by server.
            payload: serde_json::to_value(JoinRequestPayload {
                guest_name: guest_name.to_string(),
                password_hash: password_hash.to_string(),
            })?,
            timestamp: now_timestamp(),
        };
        self.ws_tx
            .send(Message::Text(serde_json::to_string(&msg)?.into()))?;
        Ok(())
    }

    /// Send an SDP offer to the Host via the signaling server.
    pub fn send_sdp_offer(
        &self,
        room_id: &str,
        guest_id: &str,
        sdp: &str,
    ) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
        let msg = SignalingMessage {
            msg_type: MsgType::SdpOffer,
            room_id: room_id.to_string(),
            sender_id: guest_id.to_string(),
            payload: serde_json::to_value(SdpPayload {
                sdp: sdp.to_string(),
            })?,
            timestamp: now_timestamp(),
        };
        self.ws_tx
            .send(Message::Text(serde_json::to_string(&msg)?.into()))?;
        Ok(())
    }

    /// Send an ICE candidate to the Host via the signaling server.
    pub fn send_ice_candidate(
        &self,
        room_id: &str,
        guest_id: &str,
        candidate: &str,
        sdp_mid: Option<String>,
        sdp_mline_index: Option<u32>,
    ) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
        let msg = SignalingMessage {
            msg_type: MsgType::IceCandidate,
            room_id: room_id.to_string(),
            sender_id: guest_id.to_string(),
            payload: serde_json::to_value(IceCandidatePayload {
                candidate: candidate.to_string(),
                sdp_mid,
                sdp_mline_index,
            })?,
            timestamp: now_timestamp(),
        };
        self.ws_tx
            .send(Message::Text(serde_json::to_string(&msg)?.into()))?;
        Ok(())
    }

    /// Receive the next signaling event from the server.
    /// Returns None if the connection has been closed.
    pub async fn recv_event(&mut self) -> Option<SignalingEvent> {
        self.event_rx.recv().await
    }

    /// Returns a lightweight sender handle for sending signaling messages
    /// from outside the client (e.g., from Tauri commands).
    pub fn sender(&self) -> SignalingSender {
        SignalingSender {
            ws_tx: self.ws_tx.clone(),
        }
    }
}

/// Lightweight handle for sending signaling messages without owning the full client.
/// Created via `SignalingClient::sender()`. The underlying WebSocket sender is
/// reference-counted, so this can be stored in shared state independently.
pub struct SignalingSender {
    ws_tx: mpsc::UnboundedSender<Message>,
}

impl SignalingSender {
    /// Notify the host that this guest is leaving. The host closes the socket
    /// after processing the message, which also terminates the receive loop.
    pub fn leave(
        &self,
        room_id: &str,
        guest_id: &str,
    ) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
        let msg = SignalingMessage {
            msg_type: MsgType::Leave,
            room_id: room_id.to_string(),
            sender_id: guest_id.to_string(),
            payload: serde_json::json!({}),
            timestamp: now_timestamp(),
        };
        self.ws_tx
            .send(Message::Text(serde_json::to_string(&msg)?.into()))?;
        Ok(())
    }

    /// Send an SDP offer to the Host via the signaling server.
    pub fn send_sdp_offer(
        &self,
        room_id: &str,
        guest_id: &str,
        sdp: &str,
    ) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
        let msg = SignalingMessage {
            msg_type: MsgType::SdpOffer,
            room_id: room_id.to_string(),
            sender_id: guest_id.to_string(),
            payload: serde_json::to_value(SdpPayload {
                sdp: sdp.to_string(),
            })?,
            timestamp: now_timestamp(),
        };
        self.ws_tx
            .send(Message::Text(serde_json::to_string(&msg)?.into()))?;
        Ok(())
    }

    /// Send an ICE candidate to the Host via the signaling server.
    pub fn send_ice_candidate(
        &self,
        room_id: &str,
        guest_id: &str,
        candidate: &str,
        sdp_mid: Option<String>,
        sdp_mline_index: Option<u32>,
    ) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
        let msg = SignalingMessage {
            msg_type: MsgType::IceCandidate,
            room_id: room_id.to_string(),
            sender_id: guest_id.to_string(),
            payload: serde_json::to_value(IceCandidatePayload {
                candidate: candidate.to_string(),
                sdp_mid,
                sdp_mline_index,
            })?,
            timestamp: now_timestamp(),
        };
        self.ws_tx
            .send(Message::Text(serde_json::to_string(&msg)?.into()))?;
        Ok(())
    }
}

fn now_timestamp() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs()
}
