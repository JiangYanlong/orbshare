import Foundation

public enum GuestConnectionState: Equatable {
    case idle
    case connecting
    case connected(roomName: String)
    case failed(message: String)
}

public struct GuestRoomEndpoint: Equatable, Sendable {
    public var host: String
    public var port: UInt16
    public var password: String

    public init(host: String = "", port: UInt16 = 38472, password: String = "") {
        self.host = host
        self.port = port
        self.password = password
    }

    public var canJoin: Bool {
        !host.trimmingCharacters(in: .whitespacesAndNewlines).isEmpty && port > 0
    }
}

public struct GuestRoomSummary: Identifiable, Equatable, Sendable {
    public var id: String
    public var name: String
    public var host: String
    public var port: UInt16
    public var requiresPassword: Bool

    public init(
        id: String,
        name: String,
        host: String,
        port: UInt16,
        requiresPassword: Bool
    ) {
        self.id = id
        self.name = name
        self.host = host
        self.port = port
        self.requiresPassword = requiresPassword
    }
}
