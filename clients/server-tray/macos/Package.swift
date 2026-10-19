// swift-tools-version:5.9
// The macOS menu-bar companion for a natively installed plurx server
// (docs/features/DESKTOP-TRAY-IMPLEMENTATION.md, task M3). Three targets keep
// the testable parts free of a running application:
//
//   PlurxTrayCore   Foundation only: the versioned controller protocol, the
//                   bounded plurx-control runner, menu/state model, polling
//                   policy and log filtering.
//   PlurxTrayUI     AppKit: status item, template icon, menus, log and upgrade
//                   windows, SMAppService login item. Built as a library so its
//                   rendering is unit-tested without launching the app.
//   PlurxServerTray The executable: single-instance check and NSApplication run.
//
// `./build` assembles `Plurx Server.app` from the release product.
import PackageDescription

let package = Package(
    name: "PlurxServerTray",
    platforms: [.macOS(.v13)],
    products: [
        .executable(name: "PlurxServerTray", targets: ["PlurxServerTray"]),
    ],
    targets: [
        .target(name: "PlurxTrayCore", path: "Sources/PlurxTrayCore"),
        .target(
            name: "PlurxTrayUI",
            dependencies: ["PlurxTrayCore"],
            path: "Sources/PlurxTrayUI",
            linkerSettings: [
                .linkedFramework("AppKit"),
                .linkedFramework("ServiceManagement"),
            ]
        ),
        .executableTarget(
            name: "PlurxServerTray",
            dependencies: ["PlurxTrayCore", "PlurxTrayUI"],
            path: "Sources/PlurxServerTray"
        ),
        .testTarget(
            name: "PlurxTrayCoreTests",
            dependencies: ["PlurxTrayCore"],
            path: "Tests/PlurxTrayCoreTests"
        ),
        .testTarget(
            name: "PlurxTrayUITests",
            dependencies: ["PlurxTrayCore", "PlurxTrayUI"],
            path: "Tests/PlurxTrayUITests"
        ),
    ]
)
