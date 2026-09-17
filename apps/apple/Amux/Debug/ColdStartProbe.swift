import AmuxCore
import AmuxFeatures
import SwiftUI

/// A launch that exists to be timed.
///
/// Cold start cannot be measured from inside a test that is already running in
/// the process, so the app measures its own: told at launch to be the probe
/// home, it reads the pinned workload's fleet from an account store through
/// the same entry point a real cold start reads its cache through, draws the
/// plain rows, and once the frame carrying them has been shown writes what it
/// took beside the other samples. The recipe then terminates it and launches
/// it again.
///
/// The store is written by a launch of its own before any measured one, so a
/// measured launch only ever reads it.
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
            ProbeHomeScreen(rows: cachedRows)
                .onAppear { record() }
        } else if name == "probe-store" {
            Color.clear.onAppear { writeStore() }
        } else {
            EmptyView()
        }
    }

    private static let storeCache = FileManager.default
        .urls(for: .cachesDirectory, in: .userDomainMask)[0]
        .appendingPathComponent("probe-store", isDirectory: true)
    private static let storeAccount = AccountId("probe")

    /// Read once per launch, as a real launch reads its cache once. The root
    /// view's body runs again when the scene becomes active, often before the
    /// first frame is presented, and a read there would time a second store
    /// read no launch performs.
    private static let cachedRows: [AgentRow] = {
        let store = FleetStore(now: Workloads.now)
        for event in try! Bridge.cachedFleet(in: storeCache, for: storeAccount) {
            store.apply(event)
        }
        return store.rows
    }()

    /// Writes the pinned fleet into the store the measured launches read, then
    /// says so where the recipe is waiting.
    private static func writeStore() {
        let fleet = Workloads.cachedFleet()
        let remembered = Remembered(
            hosts: fleet.hosts.map(\.entry), agents: fleet.agents.map(\.agent))
        guard RememberedStoreBridge.seed(remembered, in: storeCache, for: storeAccount) else {
            return
        }
        PerfFiles.ensure()
        try? Data().write(to: PerfFiles.probeStoreWritten)
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
        var samples = [MetricSample(
            metric: .coldFirstFrameMs,
            value: seconds * 1_000,
            unit: .milliseconds,
            proxy: false,
            workload: .cachedFleet40)]
        // The store's own share, held to a budget of its own so the looser
        // number around it cannot hide a slower read. A launch that marked no
        // read leaves no sample, and the suite counts that as a failure.
        if let began = Signposts.first(.storeReadBegan),
           let ended = Signposts.first(.storeReadEnded) {
            samples.append(MetricSample(
                metric: .coldStoreReadMs,
                value: (ended - began) * 1_000,
                unit: .milliseconds,
                proxy: false,
                workload: .cachedFleet40))
        }
        PerfRun.appendColdSamples(samples)
        // And where the time went inside the launch, so a number that moves
        // says which half of the launch moved it.
        PerfRun.appendColdMarks(Signposts.marks)
    }
}
