import SwiftUI

public struct OrbShareGuestRootView: View {
    @State private var store = GuestSessionStore()

    public init() {}

    public var body: some View {
        NavigationStack {
            List {
                Section("Manual Join") {
                    TextField("Host IP", text: $store.endpoint.host)
                        .hostInputStyle()

                    TextField(
                        "Port",
                        value: $store.endpoint.port,
                        format: .number
                    )
                    .portInputStyle()

                    SecureField("Password", text: $store.endpoint.password)

                    Button {
                        store.joinManualEndpoint()
                    } label: {
                        Label("Join", systemImage: "play.circle")
                    }
                    .disabled(!store.endpoint.canJoin)
                }

                Section("LAN Rooms") {
                    if store.discoveredRooms.isEmpty {
                        ContentUnavailableView(
                            "No Rooms",
                            systemImage: "network",
                            description: Text("Scan the local network or join by IP.")
                        )
                    } else {
                        ForEach(store.discoveredRooms) { room in
                            Button {
                                store.join(room)
                            } label: {
                                RoomRow(room: room)
                            }
                        }
                    }
                }

                Section("Status") {
                    StatusRow(state: store.state)
                }
            }
            .navigationTitle("OrbShare")
            .toolbar {
                ToolbarItem(placement: scanToolbarPlacement) {
                    Button {
                        store.scanLAN()
                    } label: {
                        Label("Scan", systemImage: "dot.radiowaves.left.and.right")
                    }
                }
            }
        }
    }
}

private struct RoomRow: View {
    var room: GuestRoomSummary

    var body: some View {
        HStack {
            VStack(alignment: .leading) {
                Text(room.name)
                    .font(.headline)
                Text("\(room.host):\(room.port)")
                    .font(.caption)
                    .foregroundStyle(.secondary)
            }

            Spacer()

            if room.requiresPassword {
                Image(systemName: "lock.fill")
                    .foregroundStyle(.secondary)
            }
        }
    }
}

private struct StatusRow: View {
    var state: GuestConnectionState

    var body: some View {
        switch state {
        case .idle:
            Label("Idle", systemImage: "circle")
        case .connecting:
            Label("Connecting", systemImage: "arrow.triangle.2.circlepath")
        case .connected(let roomName):
            Label("Connected to \(roomName)", systemImage: "checkmark.circle.fill")
                .foregroundStyle(.green)
        case .failed(let message):
            Label(message, systemImage: "exclamationmark.triangle.fill")
                .foregroundStyle(.red)
        }
    }
}

#Preview {
    OrbShareGuestRootView()
}

private extension View {
    @ViewBuilder
    func hostInputStyle() -> some View {
        #if os(iOS)
        self
            .textInputAutocapitalization(.never)
            .keyboardType(.numbersAndPunctuation)
        #else
        self
        #endif
    }

    @ViewBuilder
    func portInputStyle() -> some View {
        #if os(iOS)
        self.keyboardType(.numberPad)
        #else
        self
        #endif
    }
}

private var scanToolbarPlacement: ToolbarItemPlacement {
    #if os(iOS)
    .topBarTrailing
    #else
    .automatic
    #endif
}
