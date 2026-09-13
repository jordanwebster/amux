import SwiftUI

struct ReportTools: ViewModifier {
    let composition: Composition

    func body(content: Content) -> some View {
        #if AMUX_DEBUG_TOOLS
        content.modifier(DebugReports(composition: composition))
        #else
        content
        #endif
    }
}
