import Foundation
import Observation

@MainActor
@Observable
public final class GuestSessionStore {
    public var endpoint = GuestRoomEndpoint()
    public var discoveredRooms: [GuestRoomSummary] = []
    public private(set) var state: GuestConnectionState = .idle

    public init() {}

    public func scanLAN() {
        // Native mDNS discovery will be wired after the desktop signaling
        // contract is frozen. Keep the UI flow available now.
        discoveredRooms = []
    }

    public func joinManualEndpoint() {
        guard endpoint.canJoin else {
            state = .failed(message: "Enter a host and port.")
            return
        }

        state = .connecting
    }

    public func join(_ room: GuestRoomSummary) {
        endpoint = GuestRoomEndpoint(host: room.host, port: room.port)
        state = .connecting
    }

    public func disconnect() {
        state = .idle
    }
}
