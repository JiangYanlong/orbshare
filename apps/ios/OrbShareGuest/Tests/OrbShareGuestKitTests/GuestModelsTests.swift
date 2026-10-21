import Testing
@testable import OrbShareGuestKit

@Test func endpointRequiresHostAndPort() {
    #expect(!GuestRoomEndpoint(host: "", port: 38472).canJoin)
    #expect(!GuestRoomEndpoint(host: "192.168.1.10", port: 0).canJoin)
    #expect(GuestRoomEndpoint(host: "192.168.1.10", port: 38472).canJoin)
}
