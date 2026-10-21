# OrbShare LAN Meeting Architecture

Date: 2026-05-19

This document replaces the current PoC mental model with a LAN meeting architecture. The goal is not only to show a screen, but to behave like a lightweight private LAN meeting tool: fast first frame, stable low latency, readable text, and multi-guest fanout.

## Problem Statement

The current implementation has three mixed concerns:

1. Capture, encode, transport, and quality policy are coupled in `src-tauri/src/commands.rs`.
2. There are two video paths in production code:
   - H.264 over WebRTC video track.
   - JPEG/MJPEG over WebRTC DataChannel.
3. The fallback path can connect quickly, but it cannot deliver meeting-grade quality and latency at the same time.

JPEG over DataChannel is useful for debugging because it is simple and visible, but it is the wrong final transport:

- It does not use the browser media pipeline.
- It does not get normal RTP/RTCP feedback such as PLI, FIR, NACK, jitter buffering, and bitrate adaptation.
- It costs too much CPU per frame at high quality.
- It makes frame dropping, keyframe recovery, and multi-peer fanout harder.

The final architecture should make H.264/HEVC media track the normal path, with MJPEG kept as a diagnostic or emergency fallback.

## Product Modes

Quality should be a product policy layer, not hard-coded inside capture or WebRTC modules.

| Mode | Intended User | Target | Notes |
|---|---|---|---|
| `standard` | slower networks | 960x540, 15 fps, 1.5-2.5 Mbps | usable screen sharing, lower CPU |
| `high` | default | up to 2880x1800, 20 fps, 24 Mbps | Retina-aware clear text, balanced LAN mode |
| `pro` | text-focused | up to 3840x2160, 20 fps, 32 Mbps | physical-pixel text priority mode |
| `diagnostic-mjpeg` | debug only | 720p max, 10-20 fps | fast visibility, not final quality |

The media pipeline receives a resolved `QualityProfile` and stays independent from UI choices.

## Target Runtime Pipeline

```text
Host UI
  -> QualityPolicy resolves the selected mode
  -> CaptureSession starts selected source
  -> PlatformCapture produces GPU/native frames
  -> HardwareEncoder produces realtime encoded frames
  -> MediaRouter sends frames to guests through WebRTC RTP tracks
  -> Guest browser decodes through native video pipeline
  -> InteractionRouter syncs pointer, chat, annotation, and control messages
```

### macOS Target Path

```text
ScreenCaptureKit
  -> CVPixelBuffer / IOSurface
  -> VideoToolbox H.264 or HEVC hardware encoder
  -> Annex B or AVCC normalized encoded frames
  -> WebRTC video track
```

Rules:

- Do not send full raw BGRA frames through Rust for the final path.
- Do not convert every frame to JPEG for the final path.
- Keep Objective-C / Swift exceptions away from Rust FFI boundaries. The macOS capture implementation must catch platform failures before returning to Rust.

### Windows Target Path

```text
Windows Graphics Capture
  -> D3D11 texture
  -> Media Foundation hardware H.264 encoder
  -> Encoded frames
  -> WebRTC video track
```

Rules:

- Keep textures on GPU where possible.
- Avoid CPU readback unless diagnostic mode is selected.

## Module Structure

```text
crates/
  orb-core/
    room.rs                  room state and membership
    types.rs                 shared protocol and DTOs

  orb-policy/
    quality.rs               standard/high/pro profile resolution

  orb-capture/
    lib.rs                   CaptureProvider trait and source types
    macos/
      screen_capture_kit.*   stable macOS capture bridge
      display_list.rs        source discovery
    windows/
      wgc.rs                 Windows Graphics Capture provider
      display_list.rs        source discovery

  orb-encoder/
    lib.rs                   Encoder trait and encoded frame types
    macos_videotoolbox.rs    VideoToolbox realtime encoder
    windows_mf.rs            Media Foundation realtime encoder
    software_jpeg.rs         diagnostic fallback only

  orb-media/
    router.rs                per-room media fanout
    pacing.rs                frame pacing and drop-late policy
    feedback.rs              keyframe request and bitrate feedback
    stats.rs                 latency, fps, bitrate, drop counters

  orb-webrtc/
    session.rs               PeerConnection lifecycle
    video_track.rs           RTP video sender integration
    datachannel.rs           interaction channels only

  orb-signaling/
    server.rs                WebSocket signaling host
    client.rs                guest signaling client

  orb-discovery/
    publisher.rs             mDNS publish
    scanner.rs               mDNS scan

src-tauri/
  src/commands.rs            thin command layer only

src/
  pages/Home.tsx             host controls
  pages/Viewer.tsx           desktop guest UI

viewer/
  index.html                 static browser guest
```

`orb-policy`, `orb-encoder`, and `orb-media` do not exist yet. They should be introduced before more transport tuning, otherwise the command layer will keep accumulating pipeline logic.

## Runtime Responsibilities

| Layer | Owns | Must Not Own |
|---|---|---|
| UI | selected mode, source selection, visible state | encoder internals |
| `orb-policy` | quality profile resolution | WebRTC objects |
| `orb-capture` | platform frame source | bitrate, peer state |
| `orb-encoder` | H.264/HEVC/JPEG encoding | signaling, room membership |
| `orb-media` | pacing, dropping, fanout, media stats | UI state |
| `orb-webrtc` | PeerConnection, RTP track, DataChannels | screen capture |
| `orb-signaling` | SDP/ICE/join protocol | frame transport |
| `src-tauri` | command orchestration | media algorithm details |

## Video Transport Decision

Default transport:

```text
H.264 over WebRTC video track
```

Fallback transport:

```text
MJPEG over unreliable DataChannel
```

MJPEG fallback is allowed only when:

- hardware encoder fails;
- the browser cannot negotiate H.264;
- the user selects diagnostic mode;
- development needs a simple visual sanity check.

## Low Latency Rules

The product should prefer freshness over completeness.

1. Keep only the latest frame when the sender falls behind.
2. Drop stale frames instead of queueing them.
3. Request a keyframe immediately when a guest joins or reports decode failure.
4. Use short keyframe interval on LAN high/pro modes.
5. Use WebRTC RTP media track for video so the browser can decode natively.
6. Keep DataChannel for control messages: pointer, chat, annotation, raise hand, and stats.
7. Surface real stats in UI: capture fps, encoded fps, sent fps, bitrate, dropped frames, first-frame latency, and peer RTT.

## First Frame Flow

```text
Guest clicks Connect
  -> WebSocket JoinRequest
  -> Host accepts and creates PeerConnection
  -> Host attaches video track before SDP answer
  -> Guest sets remote answer
  -> ICE connected
  -> Host sends forced keyframe
  -> Guest displays first decoded frame
```

Target acceptance:

- LAN first frame: under 2 seconds.
- Host start to local preview: under 1 second.
- After reconnect: under 2 seconds.

Waiting 60-120 seconds for the first frame is a pipeline bug, not acceptable behavior.

## Black Host Preview

The host preview being black while the guest can see content is not the desired product behavior. It can happen in a PoC if host preview is not wired to the capture stream, but the final product should show one of these states:

1. Local live preview, preferably decoded from the same capture/encode pipeline.
2. A clear "sharing active" placeholder if preview is intentionally disabled for performance.
3. An explicit error if capture is active but no frames are produced.

Do not silently show a black panel.

## Implementation Phases

### Phase 0: Stabilize Current Debug Path

Goal: stop false connected states and make test results readable.

- Fix DataChannel open-state race in the MJPEG path.
- Add explicit viewer states: signaling connected, peer connected, video channel open, first frame received.
- Add first-frame timeout and error message.
- Keep MJPEG as `diagnostic-mjpeg`, not as default final architecture.

### Phase 1: Extract Policy And Pipeline Types

Goal: make the architecture enforceable in code.

- Add `orb-policy` with `QualityTier` and `QualityProfile`.
- Add `orb-encoder` crate with `VideoEncoder` trait.
- Add `orb-media` crate with `MediaRouter`, `FramePacer`, and stats structs.
- Slim `src-tauri/src/commands.rs` so it only constructs the pipeline.

### Phase 2: Make WebRTC Video Track The Default

Goal: make browser guests receive native media, not JPEG frames.

- Move H.264 sender code behind `orb-media`.
- Force a keyframe when a peer joins.
- Add RTCP feedback handling for keyframe requests.
- Configure H.264 profile consistently between encoder and SDP.
- Keep DataChannel for interaction and stats only.

### Phase 3: Stabilize macOS Capture + Encode

Goal: reliable macOS host path.

- Replace unsafe ScreenCaptureKit FFI with a safer Swift/Objective-C bridge or fully audited Objective-C boundary.
- Ensure foreign exceptions never cross into Rust.
- Keep frame timestamps monotonic.
- Add resolution scaling before encode, not after encode.

### Phase 4: Add Quality Modes

Goal: users can choose a suitable quality without changing media code.

- Resolve mode with `orb-policy`. **Implemented:** standard/high/pro have independent profiles.
- Expose standard/high/pro in UI. **Implemented.**
- Keep the selected profile visible before sharing starts. **Implemented.**
- Add per-mode acceptance tests. **Implemented at policy unit level; end-to-end acceptance remains.**

### Phase 5: Multi-Guest LAN Meeting Hardening

Goal: stable 1 host to N guests.

- Per-peer stats and backpressure.
- Per-peer bitrate/profile downgrade when a receiver is slow.
- Host-side participant list and connection health.
- 1v5 60-minute LAN soak test.

## Acceptance Criteria

| Area | Requirement |
|---|---|
| First frame | same LAN guest sees video within 2 seconds |
| Latency | pointer movement visible within 200-300 ms on high/pro LAN |
| Quality | high mode text readable at 720p; pro mode readable at 1080p |
| Stability | no crash when capture permission, display switch, or reconnect happens |
| CPU | high mode should not rely on per-frame JPEG CPU encode |
| State | no black host preview without explicit explanation |
| Fallback | diagnostic MJPEG works but is clearly labeled as fallback |

## Current Delivery Order (2026-08-24)

### Product Packaging Constraint

OrbShare is a self-contained LAN meeting application. User-facing meeting,
member, chat, moderation, and recording experiences must open inside OrbShare.
The media path must not launch or require an external FFmpeg executable, helper
application, cloud service, or separately installed runtime. Platform media
frameworks and code shipped inside the application bundle are allowed.

### P0: Screen Text Clarity

- Keep H.264 over the WebRTC video track as the normal browser path.
- Enforce capture/send pacing at the selected profile frame rate.
- Treat VideoToolbox average bitrate as a soft target and add a quantization
  quality floor so static desktop text is not over-compressed.
- Show measured send fps, encoded bitrate, dropped frames, and participant count
  in the host UI.
- Validate on a second physical device at 100% browser zoom before moving to P1.

### P1: Participant Identity, Moderation, and Public Chat

Status on 2026-08-24: stable browser participant identity and attendance are
implemented. Host-side `Remove`, room-scoped rejoin blocking, and ordered
reliable public chat are implemented for the native host and browser viewer.
Host-side `Mute` now controls the P2 microphone relay and remains enforced by
the host even if a stale client continues sending audio packets.
New room messages can also be rendered as a local, non-destructive danmaku
overlay on the shared screen. Each device can enable or disable the overlay
without changing room chat history or another participant's preference.

- Give every connection a stable room-scoped participant id, display name,
  role, join time, and connection state.
- Add an in-app right panel with two tabs: `Participants` and `Chat`. Desktop
  keeps the panel beside the shared screen; narrow/mobile layouts use an in-app
  bottom sheet without opening another page or application.
- Send chat through an ordered reliable WebRTC DataChannel. Guests send to the
  host, the host validates and broadcasts the canonical message to every peer,
  and the host UI consumes the same event stream.
- Keep chat room-local and in memory by default. Bound message length and recent
  history, rate-limit spam, show send failures, and restore recent history after
  a short reconnect.
- Put host-only `Mute` and `Remove` actions in the participant row. `Remove`
  disconnects the peer and blocks its current room token from immediately
  rejoining. Actual audio mute takes effect after the audio track lands in P2.

### P2: Meeting Audio

Status on 2026-08-24: P2-A microphone transport is implemented with a visible,
user-initiated microphone toggle, Opus WebRTC audio relay tracks, host-side
mute/unmute state, and host-side packet dropping for muted participants. The
native host can speak and monitor meeting audio through an in-process WebRTC
peer. Browser viewers can always receive audio; microphone capture on an HTTP
LAN origin remains subject to the browser secure-context policy. P2-B gives each
of up to eight active participants a stable independent relay track, so
simultaneous speakers retain separate RTP sequence/timestamp spaces and are
mixed by the receiving browser/WebView. P2-C adds an explicit host switch for
real macOS system audio: ScreenCaptureKit supplies 48 kHz stereo PCM, OrbShare's
bundled pure-Rust Opus encoder produces 20 ms packets, and a reserved WebRTC
audio source relays them to every viewer. CoreGraphics/test/MJPEG paths reject
the option instead of silently generating fake audio, and the active source is
visible in both host and browser UI. Physical two-device audio acceptance is
still required before marking the phase complete.

- Add microphone and system-audio capture with visible permission and mute state.
- Transport audio as a WebRTC audio track; moderation commands update host-owned
  room state and the participant client state.
- Prevent hidden capture: every active microphone/system-audio source is visible
  in the meeting UI.

### P3: In-App Recording with Audio

Status on 2026-08-24: the host-side recording foundation is implemented. The
host WebView receives the same H.264 screen track used for viewers, mixes the
system-audio/participant tracks and the host microphone with the platform Web
Audio graph, then records through the bundled WebKit MediaRecorder. One-second
chunks are written incrementally by Tauri commands to `Movies/OrbShare`, so the
full meeting is never held as one browser Blob. The host UI exposes start/stop,
elapsed time, saving state, final path/size, and failures; native and browser
participants receive a visible recording indicator. Stopping the room first
finalizes any active file. A physical playable-file check, long-duration soak,
pause/resume, and Windows parity remain before P3 can be marked complete.

- Record at the host before per-peer fanout so recording quality is independent
  of a slow viewer.
- Combine the shared H.264 video with microphone/system audio into a local media
  file using platform media frameworks or code shipped in the app bundle.
- Provide start, pause, stop, duration, storage path, and failure state inside
  OrbShare. Show a recording indicator to all participants.
- Write incrementally so a long meeting does not accumulate one large memory
  buffer; finalize and verify the file when recording stops.

### P3.5: Scalable Raise-Hand Queue

Status on 2026-08-24: native and browser participants can raise or lower their
hand over the existing ordered control DataChannel. The host keeps the canonical
state and first-raised timestamp, sorts raised participants first, and can lower
an individual hand. Each action updates one member and acknowledges only that
participant; it does not rebroadcast or poll the full participant list. This
keeps control traffic bounded as meetings grow, while join/leave events remain
the only operations that require a full member snapshot. Automated transitions,
duplicate suppression, and 256-member queue ordering are covered; physical
multi-device latency acceptance remains.

### P3.6: Realtime Laser Pointer

Status on 2026-08-25: native and browser meeting viewers expose an explicit
laser-pointer toggle. Coordinates are normalized to the visible shared-media
area, throttled to 25 updates per second, and relayed by the host over the
unordered, no-retransmit pointer DataChannel so old positions cannot build up
behind a slow receiver. Rendering uses a local overlay with automatic fade-out;
desktop pointer movement and mobile touch movement share the same protocol.
Physical multi-device latency acceptance remains before the feature is marked
complete.

### P4: Sharing Coordination and Source Selection

- Add explicit display/window thumbnails and remember the last source.
- Enforce one active sharer by default. Host transfer/approval resolves sharing
  conflicts and every client receives the canonical sharing-owner state.
- Cover display removal, sleep/wake, resolution changes, and reconnect.

### P5: Local Meeting Minutes

- Feed locally recorded audio into an optional in-app transcription/minutes
  module only after recording and audio are stable.
- Preserve the no-cloud default. Any future online model must be an explicit,
  separately disclosed user choice rather than a hidden runtime dependency.
