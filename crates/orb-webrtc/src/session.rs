use std::collections::HashMap;
use std::future::Future;
use std::pin::Pin;
use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::Arc;
use std::time::Duration;

use bytes::Bytes;
use interceptor::registry::Registry;
use rtp::header::Header;
use rtp::packet::Packet;
use tokio::sync::{mpsc, Mutex};
use tracing::info;

use webrtc::api::interceptor_registry::register_default_interceptors;
use webrtc::api::{media_engine::MediaEngine, APIBuilder};
use webrtc::data_channel::data_channel_init::RTCDataChannelInit;
use webrtc::data_channel::data_channel_state::RTCDataChannelState;
use webrtc::data_channel::RTCDataChannel;
use webrtc::ice_transport::ice_candidate::RTCIceCandidateInit;
use webrtc::ice_transport::ice_server::RTCIceServer;
use webrtc::media::Sample;
use webrtc::peer_connection::configuration::RTCConfiguration;
use webrtc::peer_connection::peer_connection_state::RTCPeerConnectionState;
use webrtc::peer_connection::sdp::session_description::RTCSessionDescription;
use webrtc::peer_connection::RTCPeerConnection;
use webrtc::rtp_transceiver::rtp_codec::RTCRtpCodecCapability;
use webrtc::rtp_transceiver::rtp_codec::RTCRtpCodecParameters;
use webrtc::rtp_transceiver::rtp_codec::RTPCodecType;
use webrtc::rtp_transceiver::RTCPFeedback;
use webrtc::track::track_local::track_local_static_rtp::TrackLocalStaticRTP;
use webrtc::track::track_local::track_local_static_sample::TrackLocalStaticSample;
use webrtc::track::track_local::TrackLocalWriter;

/// Each participant is assigned one independent outbound Opus track. Separate
/// tracks preserve each speaker's RTP sequence/timestamp space and let the
/// browser/WebView perform its normal multi-track audio mixing.
// 20 viewers + ScreenCaptureKit system audio + the in-process recording
// monitor, with two spare slots for reconnect overlap. This stays above the
// room-wide viewer limit so audio cannot silently lower meeting capacity.
const AUDIO_RELAY_SLOTS: usize = 24;
pub const SYSTEM_AUDIO_SOURCE_ID: &str = "__orbshare_system_audio__";

/// Host-side WebRTC session that manages multiple peer connections (one per guest).
pub struct HostSession {
    pub peers: HashMap<String, PeerState>,
    audio_source_slots: HashMap<String, usize>,
    pub video_track: Option<Arc<TrackLocalStaticSample>>,
    pub h264_profile_level_id: String,
    event_tx: mpsc::UnboundedSender<WebRtcEvent>,
}

pub struct PeerState {
    pub pc: Arc<RTCPeerConnection>,
    pub pointer_dc: Option<Arc<RTCDataChannel>>,
    /// Reliable host-to-viewer channel for meeting lifecycle events.
    pub control_dc: Arc<RTCDataChannel>,
    pub video_dc: Arc<Mutex<Option<Arc<RTCDataChannel>>>>,
    /// One outbound Opus relay track for every possible source slot. A source's
    /// packets always use the same slot at every destination, so simultaneous
    /// speakers never share RTP sequence numbers or timestamps.
    pub audio_tracks: Vec<Arc<TrackLocalStaticRTP>>,
    pub jpeg_frame_id: AtomicU32,
}

#[derive(Debug, Default, Clone, Copy)]
pub struct JpegSendStats {
    pub peer_count: usize,
    pub sent_peer_count: usize,
    pub skipped_peer_count: usize,
    pub dropped_peer_count: usize,
    pub bytes: usize,
    pub chunks: usize,
}

pub struct WebRtcEvent {
    pub guest_id: String,
    pub kind: WebRtcEventKind,
}

pub enum WebRtcEventKind {
    Connected,
    /// ICE can temporarily enter `Disconnected` while a phone is backgrounded
    /// or changing Wi-Fi power state. The peer is still recoverable and must
    /// remain in the host's participant list.
    ConnectionInterrupted,
    /// The peer connection has permanently failed or was explicitly closed.
    Disconnected,
    IceCandidate {
        candidate: String,
        sdp_mid: Option<String>,
        sdp_mline_index: Option<u16>,
    },
    PointerData(Vec<u8>),
    /// Ordered/reliable client-to-host control payload (chat and future
    /// moderation acknowledgements).
    ControlData(Vec<u8>),
    /// Encoded Opus RTP received from one meeting participant. The app layer
    /// applies host moderation before forwarding it to the other peers.
    AudioRtp(Box<Packet>),
}

/// Register the codec set we need into a `MediaEngine`, with all H.264 payload
/// types pinned to the resolved `profile-level-id` (e.g. `420032` for
/// 1920x1200).
///
/// webrtc-rs' `register_default_codecs()` hardcodes H.264 at `42001f`
/// (Baseline Level 3.1) and never exposes 4.0/5.0. If the negotiated answer
/// advertises a lower level than the actual bitstream's SPS level, the browser
/// decoder rejects the stream and the guest never receives a first frame
/// ("first frame timeout"). Registering the target level up front — and raising
/// the offer's `profile-level-id` via `raise_offer_level` so the answer echoes
/// it — makes negotiation succeed end-to-end.
fn register_codecs_for_level(
    me: &mut MediaEngine,
    h264_level: &str,
) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    let vfb = vec![
        RTCPFeedback {
            typ: "goog-remb".to_string(),
            parameter: String::new(),
        },
        RTCPFeedback {
            typ: "ccm".to_string(),
            parameter: "fir".to_string(),
        },
        RTCPFeedback {
            typ: "nack".to_string(),
            parameter: String::new(),
        },
        RTCPFeedback {
            typ: "nack".to_string(),
            parameter: "pli".to_string(),
        },
    ];

    for codec in [
        RTCRtpCodecParameters {
            capability: RTCRtpCodecCapability {
                mime_type: "audio/opus".to_string(),
                clock_rate: 48000,
                channels: 2,
                sdp_fmtp_line: "minptime=10;useinbandfec=1".to_string(),
                rtcp_feedback: vec![],
            },
            payload_type: 111,
            ..Default::default()
        },
        RTCRtpCodecParameters {
            capability: RTCRtpCodecCapability {
                mime_type: "audio/G722".to_string(),
                clock_rate: 8000,
                channels: 0,
                sdp_fmtp_line: String::new(),
                rtcp_feedback: vec![],
            },
            payload_type: 9,
            ..Default::default()
        },
        RTCRtpCodecParameters {
            capability: RTCRtpCodecCapability {
                mime_type: "audio/PCMU".to_string(),
                clock_rate: 8000,
                channels: 0,
                sdp_fmtp_line: String::new(),
                rtcp_feedback: vec![],
            },
            payload_type: 0,
            ..Default::default()
        },
        RTCRtpCodecParameters {
            capability: RTCRtpCodecCapability {
                mime_type: "audio/PCMA".to_string(),
                clock_rate: 8000,
                channels: 0,
                sdp_fmtp_line: String::new(),
                rtcp_feedback: vec![],
            },
            payload_type: 8,
            ..Default::default()
        },
    ] {
        me.register_codec(codec, RTPCodecType::Audio)?;
    }

    for codec in [
        RTCRtpCodecParameters {
            capability: RTCRtpCodecCapability {
                mime_type: "video/VP8".to_string(),
                clock_rate: 90000,
                channels: 0,
                sdp_fmtp_line: String::new(),
                rtcp_feedback: vfb.clone(),
            },
            payload_type: 96,
            ..Default::default()
        },
        RTCRtpCodecParameters {
            capability: RTCRtpCodecCapability {
                mime_type: "video/VP9".to_string(),
                clock_rate: 90000,
                channels: 0,
                sdp_fmtp_line: "profile-id=0".to_string(),
                rtcp_feedback: vfb.clone(),
            },
            payload_type: 98,
            ..Default::default()
        },
        RTCRtpCodecParameters {
            capability: RTCRtpCodecCapability {
                mime_type: "video/VP9".to_string(),
                clock_rate: 90000,
                channels: 0,
                sdp_fmtp_line: "profile-id=1".to_string(),
                rtcp_feedback: vfb.clone(),
            },
            payload_type: 100,
            ..Default::default()
        },
        // H.264 payload types all pinned to the resolved level (e.g. `420032`).
        // Every H264 PT carries the SAME resolved level so the negotiated answer
        // level is correct regardless of which payload type the browser offers.
        // The level MUST be the exact `profile-level-id` VideoToolbox emits in
        // its SPS (`4200XX` form, constraint flags = 0x00), otherwise the
        // browser's decoder rejects the bitstream and no first frame arrives.
        RTCRtpCodecParameters {
            capability: RTCRtpCodecCapability {
                mime_type: "video/H264".to_string(),
                clock_rate: 90000,
                channels: 0,
                sdp_fmtp_line: format!(
                    "level-asymmetry-allowed=1;packetization-mode=1;profile-level-id={}",
                    h264_level
                ),
                rtcp_feedback: vfb.clone(),
            },
            payload_type: 102,
            ..Default::default()
        },
        RTCRtpCodecParameters {
            capability: RTCRtpCodecCapability {
                mime_type: "video/H264".to_string(),
                clock_rate: 90000,
                channels: 0,
                sdp_fmtp_line: format!(
                    "level-asymmetry-allowed=1;packetization-mode=1;profile-level-id={}",
                    h264_level
                ),
                rtcp_feedback: vfb.clone(),
            },
            payload_type: 125,
            ..Default::default()
        },
        RTCRtpCodecParameters {
            capability: RTCRtpCodecCapability {
                mime_type: "video/H264".to_string(),
                clock_rate: 90000,
                channels: 0,
                sdp_fmtp_line: format!(
                    "level-asymmetry-allowed=1;packetization-mode=0;profile-level-id={}",
                    h264_level
                ),
                rtcp_feedback: vfb.clone(),
            },
            payload_type: 108,
            ..Default::default()
        },
        RTCRtpCodecParameters {
            capability: RTCRtpCodecCapability {
                mime_type: "video/H264".to_string(),
                clock_rate: 90000,
                channels: 0,
                sdp_fmtp_line: format!(
                    "level-asymmetry-allowed=1;packetization-mode=0;profile-level-id={}",
                    h264_level
                ),
                rtcp_feedback: vfb.clone(),
            },
            payload_type: 127,
            ..Default::default()
        },
        RTCRtpCodecParameters {
            capability: RTCRtpCodecCapability {
                mime_type: "video/AV1".to_string(),
                clock_rate: 90000,
                channels: 0,
                sdp_fmtp_line: "profile-id=0".to_string(),
                rtcp_feedback: vfb.clone(),
            },
            payload_type: 41,
            ..Default::default()
        },
        RTCRtpCodecParameters {
            capability: RTCRtpCodecCapability {
                mime_type: "video/ulpfec".to_string(),
                clock_rate: 90000,
                channels: 0,
                sdp_fmtp_line: String::new(),
                rtcp_feedback: vec![],
            },
            payload_type: 116,
            ..Default::default()
        },
    ] {
        me.register_codec(codec, RTPCodecType::Video)?;
    }

    Ok(())
}

/// Collect every H.264 `profile-level-id` value from an SDP string (deduped
/// by first appearance). Used for diagnostics so we can confirm at a glance
/// which level(s) the negotiation actually produced.
pub fn collect_profile_levels(sdp: &str) -> Vec<String> {
    let mut seen = std::collections::HashSet::new();
    let mut out = Vec::new();
    for line in sdp.lines() {
        if line.trim_start().starts_with("a=fmtp:") {
            if let Some(pos) = line.find("profile-level-id=") {
                let start = pos + "profile-level-id=".len();
                let end = line[start..]
                    .find(';')
                    .map(|i| start + i)
                    .unwrap_or(line.len());
                let val = line[start..end].to_string();
                if seen.insert(val.clone()) {
                    out.push(val);
                }
            }
        }
    }
    out
}

/// Raise the H.264 `profile-level-id` in an *offer* SDP string to the resolved
/// level, keeping the payload type unchanged.
///
/// We cannot change the payload type: the browser's local offer is fixed and
/// it will reject any answer whose payload type is not present in its own
/// offer. Raising only the level value (with `level-asymmetry-allowed=1` from
/// the browser's offer) lets the host's answer carry the higher level while
/// staying compatible with what the browser is willing to decode.
fn raise_offer_level(sdp: String, level: &str) -> String {
    sdp.lines()
        .map(|line| {
            if line.trim_start().starts_with("a=fmtp:") && line.contains("profile-level-id=") {
                let mut result = line.to_string();
                if let Some(start) = result.find("profile-level-id=") {
                    let val_start = start + "profile-level-id=".len();
                    let val_end = result[val_start..]
                        .find(';')
                        .map(|i| val_start + i)
                        .unwrap_or(result.len());
                    result.replace_range(val_start..val_end, level);
                    return result;
                }
            }
            line.to_string()
        })
        .collect::<Vec<_>>()
        .join("\n")
}

impl HostSession {
    /// Creates a new HostSession.
    /// If `enable_video_track` is true, creates an H264 video track that will be
    /// added to every peer connection. When false (JPEG DataChannel mode), no video
    /// track is created — video is sent via DataChannel instead.
    pub fn new(
        enable_video_track: bool,
        h264_profile_level_id: &str,
    ) -> (Self, mpsc::UnboundedReceiver<WebRtcEvent>) {
        let video_track = if enable_video_track {
            Some(Arc::new(TrackLocalStaticSample::new(
                RTCRtpCodecCapability {
                    mime_type: "video/H264".to_string(),
                    clock_rate: 90000,
                    channels: 0,
                    sdp_fmtp_line: format!(
                        "level-asymmetry-allowed=1;packetization-mode=1;profile-level-id={}",
                        h264_profile_level_id
                    ),
                    rtcp_feedback: vec![
                        RTCPFeedback {
                            typ: "nack".to_string(),
                            parameter: String::new(),
                        },
                        RTCPFeedback {
                            typ: "nack".to_string(),
                            parameter: "pli".to_string(),
                        },
                        RTCPFeedback {
                            typ: "transport-cc".to_string(),
                            parameter: String::new(),
                        },
                    ],
                },
                "video".to_string(),
                "webrtc-rs".to_string(),
            )))
        } else {
            None
        };

        let (event_tx, event_rx) = mpsc::unbounded_channel();

        let session = Self {
            peers: HashMap::new(),
            audio_source_slots: HashMap::new(),
            video_track,
            h264_profile_level_id: h264_profile_level_id.to_string(),
            event_tx,
        };

        (session, event_rx)
    }

    pub fn video_track_for_send(&self) -> Option<Arc<TrackLocalStaticSample>> {
        if self.peers.is_empty() {
            return None;
        }
        self.video_track.clone()
    }

    pub fn peer_connection(&self, guest_id: &str) -> Option<Arc<RTCPeerConnection>> {
        self.peers.get(guest_id).map(|peer| peer.pc.clone())
    }

    /// Reserves one stable outbound Opus slot for a non-peer source such as
    /// the host's ScreenCaptureKit system audio.
    pub fn reserve_audio_source(&mut self, source_id: &str) -> Result<usize, String> {
        if let Some(slot) = self.audio_source_slots.get(source_id).copied() {
            return Ok(slot);
        }
        let slot = first_available_audio_slot(&self.audio_source_slots)
            .ok_or_else(|| "会议音频源数量已达到上限".to_string())?;
        self.audio_source_slots.insert(source_id.to_string(), slot);
        Ok(slot)
    }

    /// Adds a new peer (guest) to the session.
    /// Creates a PeerConnection with the given ICE servers, attaches the video track,
    /// creates a "pointer" DataChannel, and sets up event handlers.
    pub async fn add_peer(
        &mut self,
        guest_id: String,
        ice_servers: Vec<RTCIceServer>,
    ) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
        self.add_peer_internal(guest_id, ice_servers, true).await
    }

    /// Adds an audio-only host WebView peer.
    pub async fn add_audio_peer(
        &mut self,
        guest_id: String,
        ice_servers: Vec<RTCIceServer>,
    ) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
        self.add_peer_internal(guest_id, ice_servers, false).await
    }

    /// Adds the host WebView monitor peer with the same H.264 screen track and
    /// conference audio slots as viewers. The WebView keeps the video track
    /// hidden and feeds it directly to its built-in MediaRecorder, avoiding a
    /// second screen capture or any external recording helper.
    pub async fn add_monitor_peer(
        &mut self,
        guest_id: String,
        ice_servers: Vec<RTCIceServer>,
    ) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
        self.add_peer_internal(guest_id, ice_servers, true).await
    }

    async fn add_peer_internal(
        &mut self,
        guest_id: String,
        ice_servers: Vec<RTCIceServer>,
        include_video: bool,
    ) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
        let audio_source_slot = first_available_audio_slot(&self.audio_source_slots)
            .ok_or("meeting audio source capacity reached")?;
        let mut media_engine = MediaEngine::default();
        register_codecs_for_level(&mut media_engine, &self.h264_profile_level_id)?;
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

        // Add the video track only if H264 mode is enabled
        if include_video {
            if let Some(ref track) = self.video_track {
                pc.add_track(track.clone()).await?;
            }
        }

        let mut audio_tracks = Vec::with_capacity(AUDIO_RELAY_SLOTS);
        for slot in 0..AUDIO_RELAY_SLOTS {
            let audio_track = Arc::new(TrackLocalStaticRTP::new(
                RTCRtpCodecCapability {
                    mime_type: "audio/opus".to_string(),
                    clock_rate: 48_000,
                    channels: 2,
                    sdp_fmtp_line: "minptime=10;useinbandfec=1".to_string(),
                    rtcp_feedback: vec![],
                },
                format!("meeting-audio-slot-{slot}-{guest_id}"),
                "orbshare-meeting-audio".to_string(),
            ));
            pc.add_track(audio_track.clone()).await?;
            audio_tracks.push(audio_track);
        }

        // Create ordered=false, maxRetransmits=0 "pointer" DataChannel
        let dc_init = RTCDataChannelInit {
            ordered: Some(false),
            max_retransmits: Some(0),
            ..Default::default()
        };
        let dc = pc.create_data_channel("pointer", Some(dc_init)).await?;

        // Lifecycle notifications must not be confused with a transient ICE
        // disconnect. Keep them on a dedicated reliable, ordered channel.
        let control_dc = pc.create_data_channel("control", None).await?;

        // The reliable control channel is bidirectional. Viewer messages are
        // canonicalized by the host before being broadcast to the room.
        {
            let event_tx = self.event_tx.clone();
            let gid = guest_id.clone();
            control_dc.on_message(Box::new(move |msg| {
                let data = msg.data.to_vec();
                let _ = event_tx.send(WebRtcEvent {
                    guest_id: gid.clone(),
                    kind: WebRtcEventKind::ControlData(data),
                });
                Box::pin(async {})
            }));
        }

        // Set up pointer DataChannel message handler
        {
            let event_tx = self.event_tx.clone();
            let gid = guest_id.clone();
            dc.on_message(Box::new(
                move |msg| -> Pin<Box<dyn Future<Output = ()> + Send>> {
                    let data = msg.data.to_vec();
                    let _ = event_tx.send(WebRtcEvent {
                        guest_id: gid.clone(),
                        kind: WebRtcEventKind::PointerData(data),
                    });
                    Box::pin(async {})
                },
            ));
        }

        let video_dc = Arc::new(Mutex::new(None));

        // Receive the participant's browser/native microphone as encoded Opus
        // RTP. Reading is isolated in its own task so audio never blocks SDP,
        // ICE, chat, or screen-frame delivery.
        {
            let event_tx = self.event_tx.clone();
            let gid = guest_id.clone();
            pc.on_track(Box::new(move |track, _receiver, _transceiver| {
                let event_tx = event_tx.clone();
                let gid = gid.clone();
                Box::pin(async move {
                    if track.kind() != RTPCodecType::Audio {
                        return;
                    }
                    tokio::spawn(async move {
                        loop {
                            match track.read_rtp().await {
                                Ok((packet, _attributes)) => {
                                    if event_tx
                                        .send(WebRtcEvent {
                                            guest_id: gid.clone(),
                                            kind: WebRtcEventKind::AudioRtp(Box::new(packet)),
                                        })
                                        .is_err()
                                    {
                                        break;
                                    }
                                }
                                Err(error) => {
                                    tracing::debug!(
                                        "[orbshare:audio] inbound track ended for {gid}: {error}"
                                    );
                                    break;
                                }
                            }
                        }
                    });
                })
            }));
        }

        // Receive guest-created channels. The viewer creates a "video" DataChannel
        // in its SDP offer — we store it for sending JPEG frames back.
        // We do NOT create our own "video" DataChannel to avoid conflict.
        {
            let event_tx = self.event_tx.clone();
            let gid = guest_id.clone();
            let recv_video_dc = video_dc.clone();
            pc.on_data_channel(Box::new(move |dc: Arc<RTCDataChannel>| {
                let event_tx = event_tx.clone();
                let gid = gid.clone();
                let recv_video_dc = recv_video_dc.clone();
                Box::pin(async move {
                    match dc.label() {
                        "video" => {
                            tracing::debug!(
                                "[orbshare] guest video data channel received for {gid}, state={:?}",
                                dc.ready_state()
                            );
                            if dc.ready_state() == RTCDataChannelState::Open {
                                let mut slot = recv_video_dc.lock().await;
                                *slot = Some(dc.clone());
                            }
                            let dc_clone = dc.clone();
                            let slot_for_open = recv_video_dc.clone();
                            let gid_for_open = gid.clone();
                            dc.on_open(Box::new(move || {
                                let opened_gid = gid_for_open.clone();
                                let dc_for_slot = dc_clone.clone();
                                let slot = slot_for_open.clone();
                                Box::pin(async move {
                                    tracing::debug!(
                                        "[orbshare] guest video data channel open for {opened_gid}"
                                    );
                                    let mut s = slot.lock().await;
                                    *s = Some(dc_for_slot);
                                })
                            }));
                        }
                        "pointer" => {
                            dc.on_message(Box::new(move |msg| {
                                let data = msg.data.to_vec();
                                let _ = event_tx.send(WebRtcEvent {
                                    guest_id: gid.clone(),
                                    kind: WebRtcEventKind::PointerData(data),
                                });
                                Box::pin(async {})
                            }));
                        }
                        _ => {}
                    }
                })
            }));
        }

        // on_peer_connection_state_change
        {
            let event_tx = self.event_tx.clone();
            let gid = guest_id.clone();
            pc.on_peer_connection_state_change(Box::new(
                move |state: RTCPeerConnectionState| -> Pin<Box<dyn Future<Output = ()> + Send>> {
                    info!("Peer {} state: {:?}", gid, state);
                    match peer_connection_event(state) {
                        Some(WebRtcEventKind::Connected) => {
                            let _ = event_tx.send(WebRtcEvent {
                                guest_id: gid.clone(),
                                kind: WebRtcEventKind::Connected,
                            });
                        }
                        Some(WebRtcEventKind::ConnectionInterrupted) => {
                            let _ = event_tx.send(WebRtcEvent {
                                guest_id: gid.clone(),
                                kind: WebRtcEventKind::ConnectionInterrupted,
                            });
                        }
                        Some(WebRtcEventKind::Disconnected) => {
                            let _ = event_tx.send(WebRtcEvent {
                                guest_id: gid.clone(),
                                kind: WebRtcEventKind::Disconnected,
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
            let event_tx = self.event_tx.clone();
            let gid = guest_id.clone();
            pc.on_ice_candidate(Box::new(
                move |candidate| -> Pin<Box<dyn Future<Output = ()> + Send>> {
                    if let Some(c) = candidate {
                        if let Ok(json) = c.to_json() {
                            let _ = event_tx.send(WebRtcEvent {
                                guest_id: gid.clone(),
                                kind: WebRtcEventKind::IceCandidate {
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

        let peer_state = PeerState {
            pc,
            pointer_dc: Some(dc),
            control_dc,
            video_dc,
            audio_tracks,
            jpeg_frame_id: AtomicU32::new(0),
        };
        self.audio_source_slots
            .insert(guest_id.clone(), audio_source_slot);
        self.peers.insert(guest_id, peer_state);

        Ok(())
    }

    /// Relays one encoded microphone RTP packet to every destination except
    /// its source. Returns the number of destination tracks written.
    pub async fn relay_audio_rtp(&self, source_guest_id: &str, packet: &Packet) -> usize {
        let Some(source_slot) = self.audio_source_slots.get(source_guest_id).copied() else {
            tracing::debug!("[orbshare:audio] no relay slot for source {source_guest_id}");
            return 0;
        };
        let mut relayed = 0usize;
        for (guest_id, peer) in &self.peers {
            if guest_id == source_guest_id {
                continue;
            }
            let Some(audio_track) = peer.audio_tracks.get(source_slot) else {
                tracing::warn!(
                    "[orbshare:audio] destination {guest_id} has no relay slot {source_slot}"
                );
                continue;
            };
            match audio_track.write_rtp(packet).await {
                Ok(_) => relayed += 1,
                Err(error) => tracing::debug!(
                    "[orbshare:audio] relay from {source_guest_id} to {guest_id} failed: {error}"
                ),
            }
        }
        relayed
    }

    /// Wraps one raw Opus packet in RTP and relays it through the source's
    /// reserved audio slot. Sequence and timestamp are maintained by the
    /// caller so every destination observes the same continuous source clock.
    pub async fn relay_audio_payload(
        &self,
        source_id: &str,
        payload: Vec<u8>,
        sequence_number: u16,
        timestamp: u32,
    ) -> usize {
        let packet = Packet {
            header: Header {
                version: 2,
                payload_type: 111,
                sequence_number,
                timestamp,
                ssrc: 0x4f52_4253,
                ..Default::default()
            },
            payload: Bytes::from(payload),
        };
        self.relay_audio_rtp(source_id, &packet).await
    }

    /// Sets the remote SDP offer from a peer and returns the SDP answer.
    pub async fn set_peer_sdp_offer(
        &self,
        guest_id: &str,
        sdp: String,
    ) -> Result<String, Box<dyn std::error::Error + Send + Sync>> {
        let peer = self.peers.get(guest_id).ok_or("Peer not found")?;

        // The incoming `sdp` is a JSON-wrapped RTCSessionDescription (e.g.
        // `{"type":"offer","sdp":"v=0\r\n..."}`) from the browser.  We must
        // deserialize it first, raise the level on the *raw* SDP string, then
        // re-wrap — otherwise `raise_offer_level` scans the JSON wrapper (one
        // line, no `a=fmtp:` lines visible) and does nothing.
        let mut offer = serde_json::from_str::<RTCSessionDescription>(&sdp)?;
        if !self.h264_profile_level_id.is_empty() {
            offer.sdp = raise_offer_level(offer.sdp, &self.h264_profile_level_id);
        }
        peer.pc.set_remote_description(offer).await?;

        let answer = peer.pc.create_answer(None).await?;
        peer.pc.set_local_description(answer.clone()).await?;

        let answer_json = serde_json::to_string(&answer)?;
        Ok(answer_json)
    }

    /// SDP exchange for the in-process host audio monitor. Both sides gather
    /// ICE into SDP because this path intentionally has no separate trickle
    /// signaling channel.
    pub async fn set_peer_sdp_offer_with_complete_ice(
        &self,
        guest_id: &str,
        sdp: String,
    ) -> Result<String, Box<dyn std::error::Error + Send + Sync>> {
        let peer = self.peers.get(guest_id).ok_or("Peer not found")?;
        let offer = serde_json::from_str::<RTCSessionDescription>(&sdp)?;
        peer.pc.set_remote_description(offer).await?;

        let answer = peer.pc.create_answer(None).await?;
        let mut gathering_complete = peer.pc.gathering_complete_promise().await;
        peer.pc.set_local_description(answer).await?;
        let _ = tokio::time::timeout(Duration::from_secs(3), gathering_complete.recv()).await;
        let local = peer
            .pc
            .local_description()
            .await
            .ok_or("missing local SDP")?;
        Ok(serde_json::to_string(&local)?)
    }

    pub async fn create_answer_for_peer_connection(
        pc: Arc<RTCPeerConnection>,
        sdp: String,
        h264_profile_level_id: &str,
    ) -> Result<String, Box<dyn std::error::Error + Send + Sync>> {
        // Deserialize the JSON-wrapped SDP from the browser, raise the level
        // on the raw SDP, then feed it to the peer connection.  See
        // `set_peer_sdp_offer` for why we can't call `raise_offer_level` on the
        // JSON string directly.
        let mut offer = serde_json::from_str::<RTCSessionDescription>(&sdp)?;
        if !h264_profile_level_id.is_empty() {
            offer.sdp = raise_offer_level(offer.sdp, h264_profile_level_id);
        }
        pc.set_remote_description(offer).await?;

        let answer = pc.create_answer(None).await?;
        pc.set_local_description(answer.clone()).await?;

        Ok(serde_json::to_string(&answer)?)
    }

    /// Adds an ICE candidate received from a peer.
    pub async fn add_peer_ice_candidate(
        &self,
        guest_id: &str,
        candidate: String,
        sdp_mid: Option<String>,
        sdp_mline_index: Option<u16>,
    ) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
        let peer = self.peers.get(guest_id).ok_or("Peer not found")?;

        let ice_candidate = RTCIceCandidateInit {
            candidate,
            sdp_mid,
            sdp_mline_index,
            ..Default::default()
        };
        peer.pc.add_ice_candidate(ice_candidate).await?;
        Ok(())
    }

    pub async fn add_ice_candidate_to_peer_connection(
        pc: Arc<RTCPeerConnection>,
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
        pc.add_ice_candidate(ice_candidate).await?;
        Ok(())
    }

    /// Writes an encoded video frame to the shared video track.
    /// frame_data should be an H264 NAL unit / access unit.
    /// Returns Ok(()) silently if no video track is configured (JPEG mode).
    pub async fn write_video_frame(
        &self,
        frame_data: Vec<u8>,
    ) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
        let track = match self.video_track {
            Some(ref t) => t,
            None => return Ok(()),
        };
        if self.peers.is_empty() {
            return Ok(());
        }
        let sample = Sample {
            data: Bytes::from(frame_data),
            duration: Duration::from_millis(33),
            ..Default::default()
        };
        track.write_sample(&sample).await?;
        Ok(())
    }

    /// Writes an encoded video frame with explicit duration.
    pub async fn write_video_frame_with_duration(
        &self,
        frame_data: Vec<u8>,
        duration: Duration,
    ) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
        let track = match self.video_track {
            Some(ref t) => t,
            None => return Ok(()),
        };
        if self.peers.is_empty() {
            return Ok(());
        }
        let sample = Sample {
            data: Bytes::from(frame_data),
            duration,
            ..Default::default()
        };
        track.write_sample(&sample).await?;
        Ok(())
    }

    pub async fn write_video_sample_to_track(
        track: Arc<TrackLocalStaticSample>,
        frame_data: Vec<u8>,
        duration: Duration,
    ) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
        let sample = Sample {
            data: Bytes::from(frame_data),
            duration,
            ..Default::default()
        };
        track.write_sample(&sample).await?;
        Ok(())
    }

    /// Sends a JPEG-encoded video frame to all connected peers via the "video" DataChannel.
    pub async fn send_video_frame_to_peers(
        &self,
        jpeg_data: Vec<u8>,
    ) -> Result<JpegSendStats, Box<dyn std::error::Error + Send + Sync>> {
        const MAGIC: &[u8; 4] = b"ORBJ";
        const HEADER_LEN: usize = 12;
        const MAX_CHUNK_PAYLOAD: usize = 32 * 1024;
        const MAX_BUFFERED_BYTES: usize = 384 * 1024;
        let total_chunks = jpeg_data.len().div_ceil(MAX_CHUNK_PAYLOAD);
        if total_chunks > u16::MAX as usize {
            tracing::debug!(
                "[orbshare] jpeg frame too large for chunking: {}",
                jpeg_data.len()
            );
            return Ok(JpegSendStats {
                peer_count: self.peers.len(),
                skipped_peer_count: self.peers.len(),
                bytes: jpeg_data.len(),
                chunks: total_chunks,
                ..Default::default()
            });
        }

        let mut stats = JpegSendStats {
            peer_count: self.peers.len(),
            bytes: jpeg_data.len(),
            chunks: total_chunks,
            ..Default::default()
        };

        for (guest_id, peer) in &self.peers {
            let dc = { peer.video_dc.lock().await.clone() };
            if let Some(dc) = dc {
                if dc.ready_state() != RTCDataChannelState::Open {
                    stats.skipped_peer_count += 1;
                    continue;
                }
                let buffered = dc.buffered_amount().await;
                if buffered > MAX_BUFFERED_BYTES {
                    stats.dropped_peer_count += 1;
                    if peer.jpeg_frame_id.load(Ordering::Relaxed) % 30 == 0 {
                        tracing::debug!(
                            "[orbshare] dropping jpeg frame for {guest_id}: data channel buffered={buffered}"
                        );
                    }
                    continue;
                }
                let frame_id = peer.jpeg_frame_id.fetch_add(1, Ordering::Relaxed);
                if frame_id < 3 || frame_id % 30 == 0 {
                    tracing::debug!(
                        "[orbshare] sending frame {frame_id} to {guest_id}, {} bytes, dc ready_state={:?}",
                        jpeg_data.len(),
                        dc.ready_state()
                    );
                }

                let mut sent_all_chunks = true;
                for (chunk_index, chunk) in jpeg_data.chunks(MAX_CHUNK_PAYLOAD).enumerate() {
                    let mut packet = Vec::with_capacity(HEADER_LEN + chunk.len());
                    packet.extend_from_slice(MAGIC);
                    packet.extend_from_slice(&frame_id.to_be_bytes());
                    packet.extend_from_slice(&(total_chunks as u16).to_be_bytes());
                    packet.extend_from_slice(&(chunk_index as u16).to_be_bytes());
                    packet.extend_from_slice(chunk);

                    let data = bytes::Bytes::from(packet);
                    match tokio::time::timeout(Duration::from_millis(150), dc.send(&data)).await {
                        Ok(Ok(_)) => {}
                        Ok(Err(e)) => {
                            tracing::debug!("[orbshare] failed to send chunk to {guest_id}: {e}");
                            sent_all_chunks = false;
                            break;
                        }
                        Err(_) => {
                            tracing::debug!("[orbshare] timed out sending chunk to {guest_id}");
                            sent_all_chunks = false;
                            break;
                        }
                    }
                }
                if sent_all_chunks {
                    stats.sent_peer_count += 1;
                } else {
                    stats.dropped_peer_count += 1;
                }
            } else {
                stats.skipped_peer_count += 1;
            }
        }
        Ok(stats)
    }

    /// Sends a reliable lifecycle message to every viewer whose control
    /// channel is open. Returns the number of viewers notified immediately;
    /// browser viewers also have an HTTP long-poll fallback at the app layer.
    pub async fn broadcast_control_message(&self, message: &str) -> usize {
        let mut notified = 0usize;
        for (guest_id, peer) in &self.peers {
            if peer.control_dc.ready_state() != RTCDataChannelState::Open {
                tracing::debug!(
                    "[orbshare] control channel not open for {guest_id}: {:?}",
                    peer.control_dc.ready_state()
                );
                continue;
            }
            match tokio::time::timeout(
                Duration::from_millis(250),
                peer.control_dc.send_text(message.to_string()),
            )
            .await
            {
                Ok(Ok(_)) => notified += 1,
                Ok(Err(error)) => tracing::debug!(
                    "[orbshare] failed to send control message to {guest_id}: {error}"
                ),
                Err(_) => {
                    tracing::debug!("[orbshare] timed out sending control message to {guest_id}")
                }
            }
        }
        notified
    }

    /// Broadcasts one normalized laser-pointer coordinate to every open
    /// viewer pointer channel. Pointer traffic is intentionally sent over the
    /// unordered, no-retransmit DataChannel so stale cursor positions never
    /// queue behind a slow receiver.
    pub async fn broadcast_pointer_data(
        &self,
        data: &[u8],
        except_guest_id: Option<&str>,
    ) -> usize {
        if data.len() != 8 {
            return 0;
        }
        let payload = Bytes::copy_from_slice(data);
        let mut sends = tokio::task::JoinSet::new();
        for (guest_id, peer) in &self.peers {
            if except_guest_id == Some(guest_id.as_str()) {
                continue;
            }
            let Some(pointer_dc) = peer.pointer_dc.as_ref() else {
                continue;
            };
            if pointer_dc.ready_state() != RTCDataChannelState::Open {
                continue;
            }
            let guest_id = guest_id.clone();
            let pointer_dc = Arc::clone(pointer_dc);
            let payload = payload.clone();
            sends.spawn(async move {
                match tokio::time::timeout(Duration::from_millis(30), pointer_dc.send(&payload))
                    .await
                {
                    Ok(Ok(_)) => true,
                    Ok(Err(error)) => {
                        tracing::debug!(
                            "[orbshare] failed to send laser pointer to {guest_id}: {error}"
                        );
                        false
                    }
                    Err(_) => {
                        tracing::debug!("[orbshare] timed out sending laser pointer to {guest_id}");
                        false
                    }
                }
            });
        }

        let mut notified = 0usize;
        while let Some(result) = sends.join_next().await {
            match result {
                Ok(true) => notified += 1,
                Ok(false) => {}
                Err(error) => {
                    tracing::debug!("[orbshare] laser pointer send task failed: {error}")
                }
            }
        }
        notified
    }

    /// Sends one reliable control message to a specific viewer.
    pub async fn send_control_message(&self, guest_id: &str, message: &str) -> Result<(), String> {
        let peer = self
            .peers
            .get(guest_id)
            .ok_or_else(|| format!("viewer {guest_id} not found"))?;
        if peer.control_dc.ready_state() != RTCDataChannelState::Open {
            return Err(format!("viewer {guest_id} control channel is not open"));
        }
        tokio::time::timeout(
            Duration::from_millis(300),
            peer.control_dc.send_text(message.to_string()),
        )
        .await
        .map_err(|_| format!("sending control message to {guest_id} timed out"))?
        .map_err(|error| format!("failed to send control message to {guest_id}: {error}"))?;
        Ok(())
    }

    /// Removes a peer and closes its PeerConnection.
    pub async fn remove_peer(
        &mut self,
        guest_id: &str,
    ) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
        self.audio_source_slots.remove(guest_id);
        if let Some(peer) = self.peers.remove(guest_id) {
            // Remove the peer from the send path before closing it. Waiting for
            // `RTCPeerConnection::close` while the caller holds HostSession's
            // write lock can deadlock with the connection-state callback,
            // which also needs to process the same peer. Closing in a detached
            // task makes host-side removal immediate and keeps the callback
            // free to finish its cleanup.
            let pc = peer.pc.clone();
            let removed_guest_id = guest_id.to_string();
            tokio::spawn(async move {
                match tokio::time::timeout(Duration::from_secs(2), pc.close()).await {
                    Ok(Ok(())) => info!("Closed removed peer: {}", removed_guest_id),
                    Ok(Err(error)) => tracing::warn!(
                        "Failed to close removed peer {}: {}",
                        removed_guest_id,
                        error
                    ),
                    Err(_) => {
                        tracing::warn!("Timed out closing removed peer: {}", removed_guest_id)
                    }
                }
            });
            info!("Removed peer from active session: {}", guest_id);
        }
        Ok(())
    }

    /// Returns a list of connected guest IDs.
    pub fn peer_ids(&self) -> Vec<String> {
        self.peers.keys().cloned().collect()
    }

    pub async fn ready_video_peer_count(&self) -> usize {
        let mut count = 0;
        for peer in self.peers.values() {
            let dc = { peer.video_dc.lock().await.clone() };
            if let Some(dc) = dc {
                if dc.ready_state() == RTCDataChannelState::Open {
                    count += 1;
                }
            }
        }
        count
    }
}

fn first_available_audio_slot(assigned: &HashMap<String, usize>) -> Option<usize> {
    (0..AUDIO_RELAY_SLOTS).find(|slot| !assigned.values().any(|assigned| assigned == slot))
}

fn peer_connection_event(state: RTCPeerConnectionState) -> Option<WebRtcEventKind> {
    match state {
        RTCPeerConnectionState::Connected => Some(WebRtcEventKind::Connected),
        RTCPeerConnectionState::Disconnected => Some(WebRtcEventKind::ConnectionInterrupted),
        RTCPeerConnectionState::Failed | RTCPeerConnectionState::Closed => {
            Some(WebRtcEventKind::Disconnected)
        }
        _ => None,
    }
}

#[cfg(test)]
mod negotiation_tests {
    use super::*;

    async fn build_pc(h264_level: &str) -> Arc<RTCPeerConnection> {
        let mut me = MediaEngine::default();
        register_codecs_for_level(&mut me, h264_level).unwrap();
        let registry = register_default_interceptors(Registry::new(), &mut me).unwrap();
        let api = APIBuilder::new()
            .with_media_engine(me)
            .with_interceptor_registry(registry)
            .build();
        let pc = Arc::new(
            api.new_peer_connection(RTCConfiguration::default())
                .await
                .unwrap(),
        );
        let track = Arc::new(TrackLocalStaticSample::new(
            RTCRtpCodecCapability {
                mime_type: "video/H264".to_string(),
                clock_rate: 90000,
                channels: 0,
                sdp_fmtp_line: format!(
                    "level-asymmetry-allowed=1;packetization-mode=1;profile-level-id={}",
                    h264_level
                ),
                rtcp_feedback: vec![],
            },
            "video".to_string(),
            "webrtc-rs".to_string(),
        ));
        pc.add_track(track).await.unwrap();
        pc
    }

    fn extract_levels(sdp: &str) -> Vec<String> {
        collect_profile_levels(sdp)
    }

    #[tokio::test]
    async fn answer_echoes_raised_offer_level() {
        // "Browser" side: default codecs (Baseline 3.1 = 42001f) makes the offer.
        let browser = build_pc("42001f").await;
        let offer = browser.create_offer(None).await.unwrap();

        // Browser sends the offer as JSON (exactly like production viewer code):
        //   JSON.stringify(offer) → {"type":"offer","sdp":"v=0\r\n..."}
        let offer_json = serde_json::to_string(&offer).unwrap();

        // "Host" side: registers the 720p H264 profile at Level 3.1.
        let host = build_pc("42001f").await;

        // Production path: create_answer_for_peer_connection receives the
        // JSON-wrapped SDP, must extract raw SDP, raise, then negotiate.
        let answer_json = HostSession::create_answer_for_peer_connection(
            host.clone(),
            offer_json.clone(),
            "42001f",
        )
        .await
        .unwrap();

        let answer: RTCSessionDescription = serde_json::from_str(&answer_json).unwrap();

        println!("[TEST] offer levels:  {:?}", extract_levels(&offer.sdp));
        println!("[TEST] answer levels: {:?}", extract_levels(&answer.sdp));

        // The answer must carry 42001f (Level 3.1).
        assert!(
            answer.sdp.contains("profile-level-id=42001f"),
            "answer should carry level 42001f, got levels: {:?}\nanswer sdp:\n{}",
            extract_levels(&answer.sdp),
            answer.sdp
        );
    }

    #[tokio::test]
    async fn answer_raises_browser_offer_to_level_4_for_1080p() {
        let browser = build_pc("42001f").await;
        let offer = browser.create_offer(None).await.unwrap();
        let offer_json = serde_json::to_string(&offer).unwrap();
        let host = build_pc("420028").await;

        let answer_json =
            HostSession::create_answer_for_peer_connection(host.clone(), offer_json, "420028")
                .await
                .unwrap();
        let answer: RTCSessionDescription = serde_json::from_str(&answer_json).unwrap();

        assert!(
            answer.sdp.contains("profile-level-id=420028"),
            "1080p answer should carry level 4.0, got levels: {:?}\nanswer sdp:\n{}",
            extract_levels(&answer.sdp),
            answer.sdp
        );
    }

    #[tokio::test]
    async fn answer_raises_browser_offer_to_level_5_for_retina_capture() {
        let browser = build_pc("42001f").await;
        let offer = browser.create_offer(None).await.unwrap();
        let offer_json = serde_json::to_string(&offer).unwrap();
        let host = build_pc("420032").await;

        let answer_json =
            HostSession::create_answer_for_peer_connection(host.clone(), offer_json, "420032")
                .await
                .unwrap();
        let answer: RTCSessionDescription = serde_json::from_str(&answer_json).unwrap();

        assert!(
            answer.sdp.contains("profile-level-id=420032"),
            "Retina answer should carry level 5.0, got levels: {:?}\nanswer sdp:\n{}",
            extract_levels(&answer.sdp),
            answer.sdp
        );
    }

    #[tokio::test]
    async fn answer_raises_browser_offer_to_level_5_1_for_4k_text_mode() {
        let browser = build_pc("42001f").await;
        let offer = browser.create_offer(None).await.unwrap();
        let offer_json = serde_json::to_string(&offer).unwrap();
        let host = build_pc("420033").await;

        let answer_json =
            HostSession::create_answer_for_peer_connection(host.clone(), offer_json, "420033")
                .await
                .unwrap();
        let answer: RTCSessionDescription = serde_json::from_str(&answer_json).unwrap();

        assert!(
            answer.sdp.contains("profile-level-id=420033"),
            "4K answer should carry level 5.1, got levels: {:?}\nanswer sdp:\n{}",
            extract_levels(&answer.sdp),
            answer.sdp
        );
    }
}

#[cfg(test)]
mod connection_state_tests {
    use super::*;

    #[test]
    fn audio_slots_use_the_first_gap_and_report_capacity() {
        let mut assigned = HashMap::from([
            ("viewer-a".to_string(), 0usize),
            ("viewer-c".to_string(), 2usize),
        ]);
        assert_eq!(first_available_audio_slot(&assigned), Some(1));

        assigned.clear();
        for slot in 0..AUDIO_RELAY_SLOTS {
            assigned.insert(format!("viewer-{slot}"), slot);
        }
        assert_eq!(first_available_audio_slot(&assigned), None);
    }

    #[test]
    fn reserved_audio_source_is_stable_and_consumes_one_slot() {
        let (mut session, _events) = HostSession::new(true, "420032");
        let system_slot = session
            .reserve_audio_source(SYSTEM_AUDIO_SOURCE_ID)
            .expect("reserve system audio");
        assert_eq!(system_slot, 0);
        assert_eq!(
            session
                .reserve_audio_source(SYSTEM_AUDIO_SOURCE_ID)
                .expect("reserve same source again"),
            system_slot
        );
        assert_eq!(
            first_available_audio_slot(&session.audio_source_slots),
            Some(1)
        );
    }

    #[test]
    fn transient_disconnect_keeps_peer_recoverable() {
        assert!(matches!(
            peer_connection_event(RTCPeerConnectionState::Disconnected),
            Some(WebRtcEventKind::ConnectionInterrupted)
        ));
    }

    #[test]
    fn failed_and_closed_connections_are_terminal() {
        for state in [
            RTCPeerConnectionState::Failed,
            RTCPeerConnectionState::Closed,
        ] {
            assert!(matches!(
                peer_connection_event(state),
                Some(WebRtcEventKind::Disconnected)
            ));
        }
    }
}
