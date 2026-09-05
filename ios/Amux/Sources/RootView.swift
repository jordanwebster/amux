import AmuxCore
import SwiftUI

/// The shell's root. Screens are hung off it as they are built; for now it
/// shows that the app is linked against the shared Rust library.
struct RootView: View {
    var body: some View {
        VStack(spacing: 8) {
            Text("root.title")
                .accessibilityIdentifier("root.title")
            Text(verbatim: Bridge.version)
                .accessibilityIdentifier("root.bridgeVersion")
        }
        .accessibilityIdentifier("root")
    }
}
