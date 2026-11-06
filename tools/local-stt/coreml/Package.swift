// swift-tools-version: 6.2
import PackageDescription

let package = Package(
    name: "xflow-stt-coreml",
    platforms: [
        .macOS(.v14)
    ],
    products: [
        .executable(name: "xflow-stt-coreml", targets: ["xflow-stt-coreml"])
    ],
    dependencies: [
        .package(
            url: "https://github.com/FluidInference/FluidAudio",
            revision: "ee78635d23d52379117554ba72358b65a0612447",
            traits: []
        )
    ],
    targets: [
        .executableTarget(
            name: "xflow-stt-coreml",
            dependencies: [
                .product(name: "FluidAudio", package: "FluidAudio")
            ],
            path: "Sources/xflow-stt-coreml"
        )
    ]
)
