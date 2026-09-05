import AmuxMobile
import Foundation

/// The shared Rust library this build is linked against. Swift never
/// reimplements protocol, projection or send-gate behaviour; it reads it here.
public enum Bridge {
    public static var version: String {
        guard let version = amux_mobile_version() else { return "" }
        return String(cString: version)
    }

    /// Which of the two libraries this binary linked. The version alone is the
    /// shipping library; the version with `+debug-tools` is the one built with
    /// the driving tools, which accepts a plaintext relay on this machine and
    /// can freeze a recorder snapshot. Reading it costs nothing and is the
    /// only way an app can tell from the inside.
    public static var build: String {
        guard let build = amux_mobile_build() else { return "" }
        return String(cString: build)
    }

    // The suffix `build` carries when the driving tools are compiled in is
    // deliberately not spelled anywhere in Swift. It is a string literal only
    // the driving library contains, and `wt run ios-door-smoke` proves a
    // release binary does not carry it by searching the bytes — a constant
    // here would be found by that search and the check would be worthless.
}

extension Bridge {
    /// The fleet this device last displayed, read straight off disk.
    ///
    /// A cold launch has rows to draw long before it has a connection, and
    /// starting the runtime to get them would put the network's setup in front
    /// of the first frame. This is the same answer the running library delivers
    /// first — every card marked as awaiting its machine, the fleet as a whole
    /// unreconciled — read by the same code, so the screen a launch draws and
    /// the screen a connection replaces it with cannot disagree.
    public static func cachedFleet(in directory: URL) -> [Event] {
        guard let json = amux_mobile_cached_fleet(directory.path) else { return [] }
        defer { amux_mobile_free(json) }
        let data = Data(String(cString: json).utf8)
        return (try? AmuxJSON.decoder.decode([Event].self, from: data)) ?? []
    }
}
