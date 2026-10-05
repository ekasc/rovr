// swift-tools-version: 5.9
import PackageDescription

let package = Package(
    name: "RovrGui",
    platforms: [.macOS(.v13)],
    products: [
        .executable(name: "RovrGui", targets: ["RovrGui"]),
    ],
    targets: [
        .executableTarget(name: "RovrGui", path: "Sources/RovrGui"),
    ]
)
