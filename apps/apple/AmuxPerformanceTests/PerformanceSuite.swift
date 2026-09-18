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
            // Each launch leaves its first frame and the store read inside it.
            let cold = PerfRun.coldSamples()
            let frames = cold.filter { $0.metric == .coldFirstFrameMs }
            let reads = cold.filter { $0.metric == .coldStoreReadMs }
            XCTAssertGreaterThanOrEqual(
                frames.count, samples,
                "the recipe launched the cold probe \(frames.count) times, not \(samples)")
            XCTAssertEqual(
                reads.count, frames.count,
                "\(frames.count) cold launches marked \(reads.count) store reads")
            for sample in frames.suffix(samples) + reads.suffix(samples) { run.record(sample) }
        }

        if inputs.measures(.reconciliation) {
            for workload in [Workload.latency0, .latency100] {
                for _ in 0..<samples {
                    run.record(try await reconciliation(latency: workload))
                    await Harness.settleAfterSample()
                }
            }
        }

        if inputs.measures(.echo) {
            for _ in 0..<samples {
                run.record(try await echo())
                await Harness.settleAfterSample()
            }
        }

        if inputs.measures(.streaming) {
            for _ in 0..<samples {
                for sample in try await streamingScroll() { run.record(sample) }
                await Harness.settleAfterSample()
            }

            for _ in 0..<samples {
                run.record(try await idle())
                await Harness.settleAfterSample()
            }
        }

        if inputs.measures(.lifecycle) {
            // The only numbers in a run the app does not take about itself.
            // How many connections a host is holding is a fact about the far
            // end of the network, and being put away and picked up is done to
            // an app rather than by it, so the Mac takes these against a relay
            // and machines it is really running and leaves them here. They are
            // judged with the rest, against the same table.
            let cycles = PerfRun.lifecycleSamples()
            XCTAssertGreaterThanOrEqual(
                cycles.count, samples * 3,
                "the recipe left \(cycles.count) lifecycle samples, not \(samples) of each of "
                + "the three the table judges")
            for sample in cycles { run.record(sample) }
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
        await harness.deliver(cached)
        await harness.settle()

        let delivery = harness.deliver(
            connected, then: confirmed, after: .milliseconds(delay))
        try await harness.wait(for: .reconciled)
        await delivery.value

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
            self.page(
                harness, agent: agent, model: model,
                identifierPrefix: "transcript.prompt",
                includeIdentifierGeometry: false
            ) { drawn.record($0) }
        }
        defer { window.isHidden = true }
        await harness.deliver(Harness.encoded([
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
        let frame = EchoFrame()
        let cancel = Signposts.observeNext(.echoCommitted) { frame.record($0, drawn: drawn) }
        defer { cancel() }
        XCTAssertTrue(harness.stores.send(to: agent), "the send never happened")
        try await harness.wait(for: .echoCommitted)

        let tapped = try XCTUnwrap(Signposts.first(.sendTapped))
        let committed = try frame.committed(carrying: text)
        return MetricSample(
            metric: .echoFrames,
            value: (committed - tapped) * 1_000,
            unit: .milliseconds,
            // One frame of this simulator is 17 ms where a ProMotion phone's
            // is 8.3, so the budget met here stands in for the phone's.
            proxy: true,
            workload: .conversation1000)
    }

    @MainActor
    func testEchoMeasurementRejectsARowDelayedByOneCommit() async throws {
        let harness = try Harness()
        defer { harness.stop() }
        Signposts.reset()
        let drawn = DrawnElements()
        let frame = EchoFrame()
        let text = "one commit too late"
        let cancel = Signposts.observeNext(.echoCommitted) { frame.record($0, drawn: drawn) }
        defer { cancel() }
        Signposts.emitWhenDrawn(.echoCommitted)
        try await harness.wait(for: .echoCommitted)
        XCTAssertThrowsError(try frame.committed(carrying: text))

        let cancelLater = Signposts.observeNext(.transcriptCommit) { _ in
            drawn.record([IdentifiedElement(
                identifier: "transcript.prompt", label: text,
                frame: CGRect(x: 0, y: 0, width: 200, height: 44))])
        }
        defer { cancelLater() }
        Signposts.emitWhenDrawn(.transcriptCommit)
        try await harness.wait(for: .transcriptCommit)
        XCTAssertEqual(drawn.transcriptRows.first?.label, text)
        XCTAssertThrowsError(try frame.committed(carrying: text)) { error in
            XCTAssertEqual(
                String(describing: error), "the marked echo frame did not carry the sent row")
        }
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
        // Build the runtime's already-encoded input before the final settle.
        // Encoding a thousand callback payloads on the main actor after that
        // settle can leave unrelated SwiftUI work waiting when frame
        // accounting begins.
        var batches: [String] = []
        var position: UInt64 = 1_000
        for row in Workloads.stream(agent: agent).lazy.flatMap({ $0 }) {
            batches.append(Harness.encoded([Workloads.append([row], to: agent, at: position)]))
            position += 1
        }
        // The screen reads the conversation store the runtime's own events
        // land in, so a row's journey from the bridge to a drawn view is the
        // app's whole journey rather than a shortcut the test took.
        let model = harness.stores.conversation(agent)
        let window = harness.show { self.page(harness, agent: agent, model: model) }
        defer { window.isHidden = true }
        var initial = Harness.encoded([
            .session(Sessions.claude(agent: agent)),
            Workloads.append(Workloads.conversation(agent: agent, rows: 1_000), to: agent, at: 0),
        ])
        await harness.deliver(initial)
        initial.removeAll(keepingCapacity: false)
        await harness.settle()

        let frames = FrameWatch()
        let cpu = CPUWatch()
        frames.start()
        let interval = Duration.seconds(1) / 50
        do {
            let delivery = harness.deliver(batches, every: interval)
            await delivery.value
        }
        // The store keeps every row synchronously and coalesces only view
        // invalidations. Leave longer than that bound plus one natural display
        // interval for the final publication and draw before stopping either
        // clock. `Harness.settle` creates its own display links; starting them
        // inside missed-frame accounting would make the instrument perturb the
        // cadence it is measuring.
        try await Task.sleep(for: .milliseconds(50))
        let hitch = frames.stop()
        let percent = cpu.percent()

        // Footprint belongs to the app, not to the workload generator. The
        // corrected generator's rows and one JSON string per callback are much
        // larger than the malformed data the original baseline happened to
        // retain. Production has only the decoded store at this point, so
        // release the driver's duplicate input and completed task before
        // asking the process how much memory the shipped screen occupies.
        batches.removeAll(keepingCapacity: false)
        await Task.yield()
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

    /// The page a measured conversation is drawn on: the shipped conversation,
    /// which is what the app pushes when somebody opens an agent.
    ///
    /// The session is delivered with the rows rather than left out, because
    /// the gate is what decides whether the composer is on the screen at all.
    /// Without it the layer is unavailable, the foot draws nothing, and every
    /// streaming number would be taken over a page missing the strip and the
    /// box that sit under the arriving rows.
    @MainActor
    private func page(
        _ harness: Harness, agent: AgentId, model: ConversationStore,
        identifierPrefix: String = "transcript.",
        includeIdentifierGeometry: Bool = true,
        drew: (@Sendable ([IdentifiedElement]) -> Void)? = nil
    ) -> some View {
        BenchConversationScreen(
            model: model,
            subject: ConversationSubject(
                name: "measured", host: "bench", directory: "~/src/amux",
                age: "2m", working: "12s"),
            identifierPrefix: identifierPrefix,
            includeIdentifierGeometry: includeIdentifierGeometry,
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
        await harness.deliver(Harness.encoded([
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
