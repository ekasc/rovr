// swift-tools-version: 5.9
import PackageDescription

let package = Package(
    name: "RovrMenuBar",
    platforms: [.macOS(.v13)],
    products: [
        .executable(name: "RovrMenuBar", targets: ["RovrMenuBar"]),
    ],
    targets: [
        .executableTarget(name: "RovrMenuBar", path: "Sources/RovrMenuBar"),
    ]
)
