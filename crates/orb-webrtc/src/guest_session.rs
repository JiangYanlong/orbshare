use std::future::Future;
use std::pin::Pin;
use std::sync::Arc;

use interceptor::registry::Registry;
use tokio::sync::mpsc;
use tracing::info;

use webrtc::api::interceptor_registry::register_default_interceptors;
use webrtc::api::{media_engine::MediaEngine, APIBuilder};
use webrtc::data_channel::RTCDataChannel;
use webrtc::ice_transport::ice_candidate::RTCIceCandidateInit;
use webrtc::ice_transport::ice_server::RTCIceServer;
use webrtc::peer_connection::configuration::RTCConfiguration;
use webrtc::peer_connection::peer_connection_state::RTCPeerConnectionState;
use webrtc::peer_connection::sdp::session_description::RTCSessionDescription;
use webrtc::peer_connection::RTCPeerConnection;
use webrtc::rtp_transceiver::rtp_receiver::RTCRtpReceiver;
use webrtc::rtp_transceiver::RTCRtpTransceiver;
use webrtc::track::track_remote::TrackRemote;

/// Guest-side WebRTC session that connects to a single host.
pub struct GuestSession {
    pub pc: Arc<RTCPeerConnection>,
    pub pointer_dc: Option<Arc<RTCDataChannel>>,
    #[allow(dead_code)]
    event_tx: mpsc::UnboundedSender<GuestWebRtcEvent>,
}

pub struct GuestWebRtcEvent {
    pub kind: GuestEventKind,
}

pub enum GuestEventKind {
    Connected,
    Disconnected,
    SdpOffer(String),
    IceCandidate {
        candidate: String,
        sdp_mid: Option<String>,
        sdp_mline_index: Option<u16>,
    },
    VideoTrack(Arc<TrackRemote>),
    PointerData(Vec<u8>),
}

impl GuestSession {
    /// Creates a new GuestSession with the given ICE server configuration.
    /// Returns (session, event_rx) where event_rx receives events from the host.
    pub async fn new(
        ice_servers: Vec<RTCIceServer>,
    ) -> Result<
        (Self, mpsc::UnboundedReceiver<GuestWebRtcEvent>),
        Box<dyn std::error::Error + Send + Sync>,
    > {
        let mut media_engine = MediaEngine::default();
        media_engine.register_default_codecs()?;
        let registry = register_default_interceptors(Registry::new(), &mut media_engine)?;
        let api = APIBuilder::new()
            .with_media_engine(media_engine)
            .with_interceptor_registry(registry)
            .build();

        let config = RTCConfiguration {
            ice_servers,
            ..Default::default()
        };

        let pc = Arc::new(api.new_peer_connection(config).await?);

        let (event_tx, event_rx) = mpsc::unbounded_channel();

        // on_track — receive video from host
        {
            let event_tx = event_tx.clone();
            pc.on_track(Box::new(
                move |track: Arc<TrackRemote>,
                      _receiver: Arc<RTCRtpReceiver>,
                      _transceiver: Arc<RTCRtpTransceiver>|
                      -> Pin<Box<dyn Future<Output = ()> + Send>> {
                    info!("Guest received track: {}", track.kind());
                    let _ = event_tx.send(GuestWebRtcEvent {
                        kind: GuestEventKind::VideoTrack(track),
                    });
                    Box::pin(async {})
                },
            ));
        }

        // on_peer_connection_state_change
        {
            let event_tx = event_tx.clone();
            pc.on_peer_connection_state_change(Box::new(
                move |state: RTCPeerConnectionState| -> Pin<Box<dyn Future<Output = ()> + Send>> {
                    info!("Guest peer state: {:?}", state);
                    match state {
                        RTCPeerConnectionState::Connected => {
                            let _ = event_tx.send(GuestWebRtcEvent {
                                kind: GuestEventKind::Connected,
                            });
                        }
                        RTCPeerConnectionState::Disconnected
                        | RTCPeerConnectionState::Failed
                        | RTCPeerConnectionState::Closed => {
                            let _ = event_tx.send(GuestWebRtcEvent {
                                kind: GuestEventKind::Disconnected,
                            });
                        }
                        _ => {}
                    }
                    Box::pin(async {})
                },
            ));
        }

        // on_ice_candidate
        {
            let event_tx = event_tx.clone();
            pc.on_ice_candidate(Box::new(
                move |candidate| -> Pin<Box<dyn Future<Output = ()> + Send>> {
                    if let Some(c) = candidate {
                        if let Ok(json) = c.to_json() {
                            let _ = event_tx.send(GuestWebRtcEvent {
                                kind: GuestEventKind::IceCandidate {
                                    candidate: json.candidate,
                                    sdp_mid: json.sdp_mid,
                                    sdp_mline_index: json.sdp_mline_index,
                                },
                            });
                        }
                    }
                    Box::pin(async {})
                },
            ));
        }

        // on_data_channel — receive pointer channel from host
        {
            let event_tx = event_tx.clone();
            pc.on_data_channel(Box::new(
                move |dc: Arc<RTCDataChannel>| -> Pin<Box<dyn Future<Output = ()> + Send>> {
                    let label = dc.label().to_string();
                    info!("Guest received data channel: {}", label);

                    if label == "pointer" {
                        let event_tx_inner = event_tx.clone();
                        dc.on_message(Box::new(
                            move |msg| -> Pin<Box<dyn Future<Output = ()> + Send>> {
                                let data = msg.data.to_vec();
                                let _ = event_tx_inner.send(GuestWebRtcEvent {
                                    kind: GuestEventKind::PointerData(data),
                                });
                                Box::pin(async {})
                            },
                        ));
                    }

                    Box::pin(async {})
                },
            ));
        }

        let session = Self {
            pc,
            pointer_dc: None,
            event_tx,
        };

        Ok((session, event_rx))
    }

    /// Creates an SDP offer and sets it as the local description.
    /// Returns the offer SDP string (JSON-serialized RTCSessionDescription).
    pub async fn create_offer(&self) -> Result<String, Box<dyn std::error::Error + Send + Sync>> {
        let offer = self.pc.create_offer(None).await?;
        self.pc.set_local_description(offer.clone()).await?;

        let offer_json = serde_json::to_string(&offer)?;
        Ok(offer_json)
    }

    /// Sets the remote SDP answer received from the host.
    pub async fn set_sdp_answer(
        &self,
        sdp: String,
    ) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
        let answer = serde_json::from_str::<RTCSessionDescription>(&sdp)?;
        self.pc.set_remote_description(answer).await?;
        Ok(())
    }

    /// Adds an ICE candidate received from the host.
    pub async fn add_ice_candidate(
        &self,
        candidate: String,
        sdp_mid: Option<String>,
        sdp_mline_index: Option<u16>,
    ) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
        let ice_candidate = RTCIceCandidateInit {
            candidate,
            sdp_mid,
            sdp_mline_index,
            ..Default::default()
        };
        self.pc.add_ice_candidate(ice_candidate).await?;
        Ok(())
    }

    /// Sends pointer position (normalized 0.0-1.0) to the host via the pointer DataChannel.
    /// Encodes as 8 bytes: 2x f32 little-endian (x, y).
    pub async fn send_pointer(
        &self,
        x: f32,
        y: f32,
    ) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
        if let Some(ref dc) = self.pointer_dc {
            let mut payload = Vec::with_capacity(8);
            payload.extend_from_slice(&x.to_le_bytes());
            payload.extend_from_slice(&y.to_le_bytes());
            dc.send(&bytes::Bytes::from(payload)).await?;
        }
        Ok(())
    }

    /// Closes the peer connection.
    pub async fn close(&self) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
        self.pc.close().await?;
        Ok(())
    }
}
