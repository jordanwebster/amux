import AmuxDesign
import SwiftUI

// The shell is built before the screens that hang off it, so each tab and each
// page is stood up here as the plainest thing that reads the right store and
// emits the right action. They are not the design: a screen replaces the one
// named for it as it lands, and nothing else about the shell changes when it
// does — the Agents tab and the conversation have already been replaced this
// way. Nothing here is a golden's subject; captures open a screen by name.

/// A route whose screen has not been built. It says which one, because a page
/// that silently showed nothing would look like a screen that failed to load.
struct UnbuiltPage: View {
    @Environment(\.design) private var design
    let route: Route

    var body: some View {
        Text(route.name)
            .designFont(.body, design)
            .foregroundStyle(design.inkMuted.color)
            .identified("page.\(route.name)", value: route.name)
    }
}
