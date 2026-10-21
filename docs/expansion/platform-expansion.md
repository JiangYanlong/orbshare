# OrbShare Platform Expansion Start

The PRD keeps MVP focused on Windows/macOS desktop Host and Guest, while reserving iOS, Android, Linux, and domestic OS support. Expansion starts with the lowest-risk split:

## Shared Contracts First

Before native clients are built, keep these protocols stable in Rust/TypeScript:

- Discovery TXT contract: room id, name, host ip, signaling port, capabilities, password flag.
- Signaling contract: join, SDP, ICE, leave, room closed.
- Interaction contract: chat, raise hand, pointer, annotation, permission updates.
- Recording contract: guest-owned recording state and file result events.

## iOS / iPadOS Guest

Scope:

- Guest only.
- Manual IP join first, mDNS discovery second.
- Receive WebRTC video/audio.
- Chat, raise hand, pointer display, annotation display.
- Local recording after receive path is stable.

Non-goals:

- iOS Host screen sharing.
- Full classroom management.

Started in:

- `apps/ios/OrbShareGuest`

## Android Guest

Scope mirrors iOS Guest:

- Guest only.
- Manual IP join first.
- WebRTC receive.
- Interaction DataChannels.

Recommended stack:

- Kotlin + Jetpack Compose.
- Native WebRTC SDK or a maintained wrapper after protocol stabilization.

## Linux / Domestic OS

Scope:

- Guest first.
- Host capture later through PipeWire/Wayland/X11 strategy.

Risk:

- Capture APIs and packaging vary heavily by distro and domestic OS vendor.

## Cross-Platform Acceptance

Every new platform must pass:

1. Join a no-password room.
2. Join a password room.
3. Reject wrong password.
4. Receive video for 10 minutes.
5. Send chat and raise-hand events.
6. Disconnect without leaving stale host state.
