import AmuxCore
import AmuxDesign
import AmuxFeatures
import SwiftUI
import UIKit
import XCTest

@testable import Amux

/// The measured run.
///
/// Everything here is the app's own code doing the app's own work: the
/// workloads are generated from the pinned seed, handed to the runtime's own
/// callback so they are decoded, ordered and applied exactly as a relay-fed
/// batch would be, and the numbers come from the marks the app leaves in every
/// build. The Mac's part is to say which machine this is and to launch the
/// cold starts; the judging happens here, against the document.
final class PerformanceSuite: XCTestCase {
    /// Five, with the app's state reset between them, as the definitions pin.
    private let samples = requiredSamples

    override func setUp() {
        super.setUp()
        continueAfterFailure = false
    }

    @MainActor
    func testTheProbeWorkloadsMeetTheirBudgets() async throws {
        let inputs = try PerfInputs.read()
        var run = PerfRun(inputs: inputs)

        // A run can be asked for one group of measurements. What it does not
        // take, it does not judge: the verdict carries rows for the samples
        // this run took and nothing else, so a partial run can never report a
        // pass on a metric it never measured.
        if inputs.measures(.cold) {
            // The launches the recipe already did, measured by the app itself.
            let cold = PerfRun.coldSamples()
            XCTAssertGreaterThanOrEqual(
                cold.count, samples,
                "the recipe launched the cold probe \(cold.count) times, not \(samples)")
            for sample in cold.suffix(samples) { run.record(sample) }
        }

        if inputs.measures(.reconciliation) {
            for workload in [Workload.latency0, .latency100] {
                for _ in 0..<samples {
                    run.record(try await reconciliation(latency: workload))
                }
            }
        }

        if inputs.measures(.echo) {
            for _ in 0..<samples {
                run.record(try await echo())
            }
        }

        if inputs.measures(.streaming) {
            for _ in 0..<samples {
                for sample in try await streamingScroll() { run.record(sample) }
            }

            for _ in 0..<samples {
                run.record(try await idle())
            }
        }

        let cadence = FrameCadence.current()
        let verdict = try run.finish(cadence: cadence)

        // The simulator reports 60 Hz, so the readiness claim is about the app
        // not capping itself rather than about a phone's 120.
        XCTAssertTrue(
            cadence.ready,
            "the app is capping its own frame rate: \(cadence)")

        for result in verdict.results where !result.passed {
            XCTFail("\(result.metric.rawValue): \(result.note ?? "over budget")")
        }
        XCTAssertTrue(verdict.passed)
    }

    // MARK: - Reconciliation

    /// From the stream connecting to the last cached row being confirmed, with
    /// the runner's latency in front of the fleet.
    @MainActor
    private func reconciliation(latency workload: Workload) async throws -> MetricSample {
        let delay = try XCTUnwrap(workload.latencyMilliseconds)
        let harness = try Harness()
        defer { harness.stop() }
        Signposts.reset()

        // Both batches are generated and encoded before the clock starts:
        // the runtime does that work on its own thread, so doing it here
        // between the two marks would put it in the measurement.
        let cached = Harness.encoded([.fleet(Workloads.cachedFleet(reconciled: false))])
        let confirmed = Harness.encoded([.fleet(Workloads.cachedFleet(reconciled: true))])
        let connected = Harness.encoded([.connection(ConnectionUpdate(state: .connected))])

        // The cache is on screen first: what is being measured is the wait
        // between a connection and the rows it confirms, not the first draw.
        harness.deliver(cached)
        await harness.settle()

        harness.deliver(connected)
        try await Task.sleep(for: .milliseconds(delay))
        harness.deliver(confirmed)
        try await harness.wait(for: .reconciled)

        let opened = try XCTUnwrap(Signposts.first(.streamConnected))
        let reconciled = try XCTUnwrap(Signposts.first(.reconciled))
        XCTAssertTrue(harness.stores.fleet.rows.allSatisfy(\.confirmed))
        return MetricSample(
            metric: .reconciliationMs,
            value: (reconciled - opened) * 1_000,
            unit: .milliseconds,
            proxy: false,
            workload: workload)
    }

    // MARK: - The optimistic echo

    /// From a send being handled to the frame a person can see their own words
    /// in, on the page they wrote them on.
    ///
    /// Nothing has left the phone by the time that row appears: it is drawn
    /// from what was typed, and the host's own copy of it arrives later and
    /// replaces it. So this interval is the app's own work from end to end,
    /// and a slow network cannot flatter it or spoil it.
    ///
    /// It is taken over the pinned thousand-row transcript with the composer
    /// really there, because that is the page a message is sent from:
    /// appending a row to a list that already has a screenful above it, with
    /// the strip and the box laid out under it, is the work being budgeted.
    @MainActor
    private func echo() async throws -> MetricSample {
        let harness = try Harness()
        defer { harness.stop() }

        let agent = AgentId(UUID())
        let entries = Workloads.conversation(agent: agent, rows: 1_000)
        let model = harness.stores.conversation(agent)
        // The page's own account of what it drew, so the frame the echo was
        // marked in can be shown to have carried the row rather than assumed
        // to have.
        let drawn = DrawnElements()
        let window = harness.show {
            self.page(harness, agent: agent, model: model) { drawn.record($0) }
        }
        defer { window.isHidden = true }
        harness.deliver(Harness.encoded([
            .session(Sessions.claude(agent: agent)),
            Workloads.append(entries, to: agent, at: 0),
        ]))
        await harness.settle()
        XCTAssertTrue(
            model.gate.accepts,
            "the composer would not take a message, so nothing here is an echo")

        let text = "does this row arrive in the frame after the tap"
        model.draft.body = text
        // Everything before this is a person typing. The clock starts where
        // they stop, and the app's own send is what runs after it: the
        // command is built, handed to the runtime, and the row goes up.
        Signposts.reset()
        XCTAssertTrue(harness.stores.send(to: agent), "the send never happened")
        try await harness.wait(for: .echoCommitted)

        let tapped = try XCTUnwrap(Signposts.first(.sendTapped))
        let committed = try XCTUnwrap(Signposts.first(.echoCommitted))
        var carried = false
        for _ in 0..<10 {
            carried = drawn.transcriptRows.contains {
                $0.identifier == "transcript.prompt" && $0.label == text
            }
            if carried { break }
            await harness.settle()
        }
        XCTAssertTrue(carried, "the echo was marked on a page the sent row never reached")
        return MetricSample(
            metric: .echoFrames,
            value: (committed - tapped) * 1_000,
            unit: .milliseconds,
            // One frame of this simulator is 17 ms where a ProMotion phone's
            // is 8.3, so the budget met here stands in for the phone's.
            proxy: true,
            workload: .conversation1000)
    }

    // MARK: - Streaming scroll

    /// A thousand rows on screen with fifty more arriving every second for
    /// twenty seconds, while the list follows the tail.
    @MainActor
    private func streamingScroll() async throws -> [MetricSample] {
        let harness = try Harness()
        defer { harness.stop() }
        Signposts.reset()

        let agent = AgentId(UUID())
        let entries = Workloads.conversation(agent: agent, rows: 1_000)
        // The screen reads the conversation store the runtime's own events
        // land in, so a row's journey from the bridge to a drawn view is the
        // app's whole journey rather than a shortcut the test took.
        let model = harness.stores.conversation(agent)
        let window = harness.show { self.page(harness, agent: agent, model: model) }
        defer { window.isHidden = true }
        harness.deliver(Harness.encoded([
            .session(Sessions.claude(agent: agent)),
            Workloads.append(entries, to: agent, at: 0),
        ]))
        await harness.settle()

        // The rows arrive as the runtime would hand them over: fifty a
        // second, coalesced into one batch per frame rather than one lump a
        // second, because that is what the bridge's frame interval does to a
        // stream before the app ever sees it.
        let arrivals = Workloads.stream(agent: agent).flatMap { $0 }
        var batches: [String] = []
        var position = UInt64(entries.count)
        for row in arrivals {
            batches.append(Harness.encoded([Workloads.append([row], to: agent, at: position)]))
            position += 1
        }

        let frames = FrameWatch()
        let cpu = CPUWatch()
        frames.start()
        let started = ContinuousClock.now
        let interval = Duration.seconds(1) / 50
        for (index, batch) in batches.enumerated() {
            harness.deliver(batch)
            let due = started + interval * (index + 1)
            let remaining = ContinuousClock.now.duration(to: due)
            if remaining > .zero { try await Task.sleep(for: remaining) }
        }
        let hitch = frames.stop()
        let percent = cpu.percent()
        let footprint = Footprint.megabytes()

        XCTAssertEqual(
            harness.stores.conversation(agent).entries.count, 2_000,
            "the stream did not reach the transcript")
        XCTAssertGreaterThan(frames.frames, 100, "the display link saw almost no frames")
        return [
            MetricSample(
                metric: .hitchTimeRatioMsPerS, value: hitch, unit: .millisecondsPerSecond,
                // Missed-frame accounting on a simulator that composites
                // through the Mac's display: a stand-in for XCTHitchMetric.
                proxy: true, workload: .stream50PerSecond20s),
            MetricSample(
                metric: .mainThreadCpuPercent, value: percent, unit: .percent,
                proxy: false, workload: .stream50PerSecond20s),
            MetricSample(
                metric: .footprintMB, value: footprint, unit: .megabytes,
                proxy: false, workload: .conversation1000),
        ]
    }

    /// The page a measured conversation is drawn on: the shipped conversation
    /// with the fleet's drawer over it, which is what the app pushes when
    /// somebody opens an agent.
    ///
    /// The session is delivered with the rows rather than left out, because
    /// the gate is what decides whether the composer is on the screen at all.
    /// Without it the layer is unavailable, the foot draws nothing, and every
    /// streaming number would be taken over a page missing the strip and the
    /// box that sit under the arriving rows.
    @MainActor
    private func page(
        _ harness: Harness, agent: AgentId, model: ConversationStore,
        drew: (@Sendable ([IdentifiedElement]) -> Void)? = nil
    ) -> some View {
        BenchConversationScreen(
            model: model,
            fleet: harness.stores.fleet,
            hosts: harness.stores.hosts,
            subject: ConversationSubject(
                name: "measured", host: "bench", directory: "~/src/amux",
                age: "2m", working: "12s"),
            drew: drew)
    }

    // MARK: - Idle

    /// A settled screen with nothing arriving must commit nothing and ask for
    /// no frames at all.
    @MainActor
    private func idle() async throws -> MetricSample {
        let harness = try Harness()
        defer { harness.stop() }

        let agent = AgentId(UUID())
        let entries = Workloads.conversation(agent: agent, rows: 1_000)
        let model = harness.stores.conversation(agent)
        // What the list drew is read here rather than under the stream: the
        // reading is a preference travelling up the view tree, and asking for
        // it while rows arrive every twenty milliseconds puts the instrument
        // inside the thing it is measuring. A settled screen answers the same
        // question and answers it about the same list.
        let drawn = DrawnElements()
        let window = harness.show {
            self.page(harness, agent: agent, model: model) { drawn.record($0) }
        }
        defer { window.isHidden = true }
        harness.deliver(Harness.encoded([
            .session(Sessions.claude(agent: agent)),
            Workloads.append(entries, to: agent, at: 0),
        ]))
        await harness.settle()
        try await Task.sleep(for: .seconds(2))

        // A thousand rows in the transcript, of which a screenful was ever
        // drawn. A list that drew them all would meet a hitch budget for a
        // while and then run out of memory on a longer transcript, so the
        // laziness is checked rather than inferred from the footprint.
        let built = drawn.transcriptRows
        print("the transcript drew \(built.count) of its 1,000 rows")
        XCTAssertGreaterThan(built.count, 0, "the transcript drew nothing to measure")
        XCTAssertLessThan(
            built.count, 200,
            "the transcript drew \(built.count) of 1,000 rows: it is not rendering lazily")
        // A run of reads that opened itself would have drawn the lines inside
        // it, and every number here would be about an opened transcript rather
        // than the one a person arrives at.
        XCTAssertTrue(
            built.filter { $0.identifier == "transcript.exploration" }
                .allSatisfy { $0.value == "folded" },
            "a folded run of reads was open while the transcript was measured")

        Signposts.reset()
        try await Task.sleep(for: .seconds(5))
        let commits = Signposts.count(.transcriptCommit) + Signposts.count(.idleTick)
        return MetricSample(
            metric: .idleCommits, value: Double(commits), unit: .count,
            proxy: false, workload: .conversation1000)
    }
}
