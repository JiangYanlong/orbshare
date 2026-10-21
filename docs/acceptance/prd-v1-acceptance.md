# OrbShare PRD v1 Acceptance Matrix

Source: current open-source product roadmap and automated tests.

Status legend:

- `PASS`: implemented and covered by automated or manual acceptance evidence.
- `PARTIAL`: usable path exists, but PRD acceptance is incomplete.
- `TODO`: not implemented yet.
- `DEFERRED`: explicitly outside MVP or extension work.

## Automated Baseline

Run:

```bash
./script/acceptance.sh
```

Current baseline:

- TypeScript production build.
- Rust formatting check.
- Rust workspace tests.
- Tauri macOS debug `.app` bundle.
- Bundle metadata check for name, identifier, and icon.
- macOS 12.3 minimum version and Hardened Runtime signature flag.

## MVP Required Features

| ID | PRD Requirement | Current Status | Evidence / Gap |
|---|---|---:|---|
| MVP-01 | Create sharing room | PASS | `create_room`, local UI smoke, signaling server start, mDNS publish. |
| MVP-02 | LAN room auto discovery | PARTIAL | mDNS scanner/publisher exist; needs multi-device acceptance run. |
| MVP-03 | Manual IP + room code join | PARTIAL | Manual IP + port join exists; room code semantics are not complete. |
| MVP-04 | Room password | PASS | Password normalization/hash path and signaling join tests. |
| MVP-05 | Screen sharing | PARTIAL | macOS now enumerates full desktops and visible application windows with in-app previews, uses a privacy-safe desktop-independent window filter, and can switch sources without recreating the room; cross-device visual acceptance and Windows window parity remain. |
| MVP-06 | System audio sharing | PARTIAL | macOS ScreenCaptureKit PCM capture, bundled pure-Rust Opus encoding, stable WebRTC relay slot, visible host/browser state, and on/off room configuration are implemented; physical two-device listening acceptance and Windows capture remain. |
| MVP-07 | Microphone sharing | PARTIAL | User-initiated native microphone capture, Opus WebRTC relay, visible state, and host mute enforcement are implemented; physical multi-device audio acceptance and HTTP browser secure-context handling remain. |
| MVP-08 | Guest watches shared screen | PARTIAL | WebRTC/JPEG receiver exists; needs two-device e2e acceptance. |
| MVP-09 | Chat | PASS | Ordered reliable DataChannel, host canonicalization/broadcast, native/browser UI, bounded history, and rate limiting are implemented. |
| MVP-10 | Raise hand | PARTIAL | Native/browser guests send one lightweight state event; the host owns canonical state, orders raised members by time, and can lower a hand. Physical multi-device latency acceptance remains. |
| MVP-11 | Laser pointer | DEFERRED | Product decision: defer laser-pointer work until after the stable LAN-sharing release. Existing experimental path is not part of the release gate. |
| MVP-12 | Basic annotation | DEFERRED | Product decision: defer annotation until after the stable LAN-sharing release. |
| MVP-13 | Host controls annotation permission | DEFERRED | Follows the deferred annotation phase. |
| MVP-14 | Guest local recording | DEFERRED | Product decision: Guest recording is deferred. Host-side in-app recording remains in scope. |
| MVP-15 | Online participant count | PASS | Host member list/count, stable participant identity, join/leave/rejoin attendance timeline, removal synchronization, and one shared native/browser capacity gate are implemented. The current peer-to-peer safety limit is 20 viewers. |
| MVP-16 | Basic settings page | TODO | Settings route, persistence, and defaults missing. |
| MVP-17 | Open-source distribution | PASS | Public build has no activation dependency; all three quality profiles are available and private signing material is excluded. |

## PRD Acceptance Cases

| Area | Case | Expected Result | Status |
|---|---|---|---:|
| Create room | No permission for real capture | Permission guide shown | PARTIAL |
| Create room | Password room | Guests must provide password | PASS |
| Join room | Wrong password | Join rejected | PASS |
| Join room | Correct password | Guest joins | PASS at signaling layer; e2e pending |
| Video | 1080p test source | Viewer receives frames | PARTIAL |
| Video | Select full desktop | All application switching remains visible | PARTIAL — implemented on macOS; physical viewer acceptance pending |
| Video | Select one application window | Other applications are never captured | PARTIAL — implemented with ScreenCaptureKit desktop-independent window filtering; minimize/close behavior needs physical acceptance |
| Video | Switch source during a room | Room, viewers, chat, attendance, and recording session remain active | PARTIAL — capture provider swaps in place and viewers receive switching state; physical multi-viewer acceptance pending |
| Audio | System audio on/off | Guest hears/stops hearing system audio | PARTIAL — PCM conversion, Opus packetization/round-trip, relay reservation, and builds pass; physical macOS-to-phone listening acceptance pending |
| Audio | Microphone on/off | Guest hears/stops hearing mic | PARTIAL — automated state/moderation tests pass; physical two-device audio acceptance pending |
| Annotation | Host draws | Guest sees synchronized stroke | TODO |
| Annotation | Guest requests permission | Host can approve and revoke | TODO |
| Annotation | Clear annotations | All clients clear canvas | TODO |
| Chat | Send message | All clients receive ordered message | PASS |
| Raise hand | Raise/cancel | Host queue updates within 1 second | PARTIAL — state transition, duplicate suppression, 256-member ordering, host queue, and native/browser controls are implemented; physical latency acceptance remains |
| Recording | Start/stop recording | Playable local file generated | PARTIAL — host controls, MP4/WebM MediaRecorder, incremental app-owned file writing, final path/size state, and participant indicator are implemented; physical playback acceptance remains |
| Recording | 30-minute recording | Does not accumulate one large in-memory blob | PARTIAL — one-second chunks are flushed to disk without retaining the full Blob; 30-minute soak and disk-full recovery remain |
| Settings | Save defaults | Settings persist locally | TODO |
| Distribution | Fresh clone build | App builds without private modules or credentials | PASS — covered by workspace tests and the acceptance script. |

## Release Gate

MVP can be accepted only when:

1. All 17 MVP required features are `PASS`.
2. `./script/acceptance.sh` passes on macOS.
3. Windows build and smoke test pass on Windows 10/11.
4. Two-device LAN acceptance covers Host and Guest on separate machines.
5. The staged capacity run in `closed-beta-stability.md` passes: 1→5 for 60 minutes, then 1→10; 20-viewer claims require the separate extended-capacity run.
6. Screen/audio/recording actions always have visible user state.

## Next Implementation Order

1. Verify desktop/window selection and in-room source switching on a physical phone/PC viewer, including a minimized or closed selected window.
2. Verify macOS system audio on a physical phone/PC viewer and close P2 regressions.
3. Verify the host recording file on macOS with screen, microphone, system audio, and one source switch, then run a 30-minute soak.
4. Complete staged 5/10/20-viewer performance runs and use the collected CPU, memory, bitrate, frame-rate, reconnect, and audio results as the capacity release gate.
5. Settings and local persistence.
6. Windows capture/encode and window-selection parity plus two-device LAN acceptance.
