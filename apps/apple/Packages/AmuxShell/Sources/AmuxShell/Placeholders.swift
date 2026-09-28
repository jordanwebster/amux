import AmuxDesign
import SwiftUI

// A route the shell can open but no screen has been built for yet; today that
// is a host's own page. Nothing here is a golden's subject.

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
