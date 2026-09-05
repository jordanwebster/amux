import AmuxCore
import AmuxFeatures
import SwiftUI

/// A launch that exists to be timed.
///
/// Cold start cannot be measured from inside a test that is already running in
/// the process, so the app measures its own: told at launch to be the probe
/// home, it fills the fleet from the pinned workload exactly as a real cold
/// start fills it from the cache, draws the plain rows, and once the frame
/// carrying them has been shown writes what it took beside the other samples.
/// The recipe then terminates it and launches it again.
@MainActor
enum ColdStartProbe {
    /// The launch argument, as `-amux-probe probe-home`.
    static let argument = "amux-probe"

    static var requested: String? {
        UserDefaults.standard.string(forKey: argument)
    }

    @ViewBuilder
    static func view(_ name: String) -> some View {
        // One probe today. A name nobody has built draws nothing rather than
        // drawing something else, so a mistyped argument fails the run
        // instead of producing a number about the wrong screen.
        if name == "probe-home" {
            ProbeHomeScreen(rows: cachedRows())
                .onAppear { record() }
        } else {
            EmptyView()
        }
    }

    private static func cachedRows() -> [AgentRow] {
        let store = FleetStore(now: Workloads.now)
        store.apply(.fleet(Workloads.cachedFleet()))
        return store.rows
    }

    /// Waits for the mark the fleet store leaves when its first rows have been
    /// presented, then writes the launch's own sample. Waiting on the mark
    /// rather than on a delay means the number is the frame, not the timer.
    private static func record(remaining: Int = 20) {
        guard let seconds = Signposts.first(.firstCachedFrame) else {
            guard remaining > 0 else { return }
            DisplayTick.once { record(remaining: remaining - 1) }
            return
        }
        PerfRun.appendColdSample(MetricSample(
            metric: .coldFirstFrameMs,
            value: seconds * 1_000,
            unit: .milliseconds,
            proxy: false,
            workload: .cachedFleet40))
    }
}
