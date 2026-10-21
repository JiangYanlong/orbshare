// swift-tools-version: 5.9
import PackageDescription

let package = Package(
    name: "OrbShareGuest",
    platforms: [
        .iOS(.v17),
        .macOS(.v14)
    ],
    products: [
        .library(
            name: "OrbShareGuestKit",
            targets: ["OrbShareGuestKit"]
        )
    ],
    targets: [
        .target(name: "OrbShareGuestKit"),
        .testTarget(
            name: "OrbShareGuestKitTests",
            dependencies: ["OrbShareGuestKit"]
        )
    ]
)
