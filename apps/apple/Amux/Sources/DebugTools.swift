import Foundation

/// The debug tools — the driving door and everything that opens a fixture
/// without a host — are compiled only into Debug builds. Release inspection
/// asserts their absence, so the flag is read here and nowhere else.
enum DebugTools {
    static var isCompiledIn: Bool {
        #if AMUX_DEBUG_TOOLS
        true
        #else
        false
        #endif
    }
}
