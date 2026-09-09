import AmuxCore
import AmuxDesign
import AmuxFeatures
import SwiftUI
import UIKit
import XCTest

@testable import Amux

/// One measured sample's world: a running runtime, the stores its events land
/// in, and a window whatever is being measured is drawn in.
///
/// Every sample builds one and throws it away, which is what "state reset
/// between samples" means inside a process: no store, no window and no mark
/// survives into the next number.
@MainActor
final class Harness {
    let stores: StoreBundle
    private let bridge: BridgeClient
    private var pump: Task<Void, Never>?
    private var window: UIWindow?

    init() throws {
        let directories = FileManager.default
        let root = directories.temporaryDirectory
            .appendingPathComponent("perf-\(UUID().uuidString)", isDirectory: true)
        let data = root.appendingPathComponent("data", isDirectory: true)
        let cache = root.appendingPathComponent("cache", isDirectory: true)
        try directories.createDirectory(at: data, withIntermediateDirectories: true)
        try directories.createDirectory(at: cache, withIntermediateDirectories: true)
        // A relay nothing answers on: the runtime is real and started exactly
        // as the app starts it, but the events being measured are the
        // workload's rather than a network's. The address is spoken to under
        // system trust because the shipping bridge refuses plaintext, and
        // nothing here ever completes a connection anyway.
        bridge = try BridgeClient(configuration: BridgeConfiguration(
            dataDirectory: data, cacheDirectory: cache, deviceName: "performance",
            relay: BridgeConfiguration.Relay(
                url: "https://127.0.0.1:1", tls: .system),
            accounts: [BridgeConfiguration.Account(id: "performance",
                                        token: .fixed("measured"))],
            active: "performance",
            logPath: data.appendingPathComponent("perf.log")))
        stores = StoreBundle(account: AccountId("performance"), clock: { Workloads.now })
        let stores = stores
        // Sending goes out the way it does in the app: the composer's command
        // is encoded and handed to the runtime, which answers with the
        // identifier its result will carry. Nothing reaches a relay, and
        // nothing needs to — the row a send puts on screen is drawn from what
        // was typed, and measuring the echo means measuring that.
        let client = bridge
        stores.dispatch = { [weak client] command in client?.dispatch(command) }
        pump = Task { @MainActor in
            for await batch in client.events { stores.apply(batch) }
        }
    }

    /// Hands the runtime's own callback a batch, encoded as the runtime
    /// encodes it. The decoding, the ordering and the hop to the main actor
    /// are the app's, not the test's.
    func deliver(_ events: [Event]) {
        deliver(Harness.encoded(events))
    }

    /// Delivers bytes that were encoded earlier.
    ///
    /// A measured stream encodes its batches before the clock starts: the
    /// runtime produces this JSON on its own worker, so encoding it on the
    /// main thread mid-measurement would put Rust's work into the app's
    /// number.
    func deliver(_ json: String) {
        bridge.deliverAsRuntime(json)
    }

    static func encoded(_ events: [Event]) -> String {
        guard let json = try? AmuxJSON.encoder.encode(events) else {
            XCTFail("a workload that cannot be encoded is not a workload")
            return "[]"
        }
        return String(decoding: json, as: UTF8.self)
    }

    /// Puts a view on screen in its own window, key and visible, because a
    /// window nobody is showing is a window nobody is drawing.
    @discardableResult
    func show<Content: View>(@ViewBuilder _ content: () -> Content) -> UIWindow {
        let scene = UIApplication.shared.connectedScenes.compactMap { $0 as? UIWindowScene }.first
        let window = scene.map { UIWindow(windowScene: $0) } ?? UIWindow(frame: UIScreen.main.bounds)
        window.rootViewController = UIHostingController(rootView: content())
        window.makeKeyAndVisible()
        self.window = window
        return window
    }

    /// Waits until the screen has stopped changing: a state change lands one
    /// frame, is laid out the next and is drawn the third.
    func settle() async {
        for _ in 0..<3 { await frame() }
    }

    /// Waits for a mark, or fails rather than hanging until the recipe's
    /// timeout fires.
    ///
    /// A measurement that never arrives is a red run, not a skipped one: a
    /// skip leaves the suite green and the verdict short of a metric, which
    /// reads as "nothing was wrong" when in fact nothing was measured.
    func wait(for signpost: Signpost, seconds: Double = 10) async throws {
        let deadline = ContinuousClock.now + .seconds(seconds)
        while Signposts.first(signpost) == nil {
            if ContinuousClock.now > deadline {
                let why = "never reached \(signpost.rawValue) within \(seconds)s"
                XCTFail(why)
                throw StalledMeasurement(why: why)
            }
            await frame()
        }
        // The mark is left when the state changed; the frame after it is when
        // a person could see it.
        await frame()
    }

    func stop() {
        pump?.cancel()
        pump = nil
        bridge.stop()
        window?.isHidden = true
        window = nil
    }

    private func frame() async {
        await withCheckedContinuation { continuation in
            DisplayTick.once { continuation.resume() }
        }
    }
}

/// A measurement that stopped moving before it finished.
struct StalledMeasurement: Error, CustomStringConvertible {
    let why: String

    var description: String { why }
}

/// What the bench transcript last said it had drawn.
///
/// SwiftUI draws rows into layers rather than into a view each, so counting
/// the window's views says nothing about how many rows were built. What a
/// built row does do is name itself, and the names travel up the view tree as
/// a preference — so this is the list's own account of what it made.
final class DrawnElements: @unchecked Sendable {
    private let lock = NSLock()
    private var elements: [IdentifiedElement] = []

    func record(_ drawn: [IdentifiedElement]) {
        lock.lock()
        defer { lock.unlock() }
        elements = drawn
    }

    /// The transcript rows among them. Everything a row draws names itself,
    /// so a code block inside a prose row counts too; what matters is the
    /// order of magnitude, not the exact number.
    var transcriptRows: [IdentifiedElement] {
        lock.lock()
        defer { lock.unlock() }
        return elements.filter { $0.identifier.hasPrefix("transcript.") }
    }
}

/// The row evidence and timestamp belong to one commit, even if the test
/// resumes after later frames have changed the drawn tree.
final class EchoFrame: @unchecked Sendable {
    private let lock = NSLock()
    private var captured: (SignpostMark, [IdentifiedElement])?

    func record(_ mark: SignpostMark, drawn: DrawnElements) {
        let rows = drawn.transcriptRows
        lock.withLock {
            if captured == nil { captured = (mark, rows) }
        }
    }

    func committed(carrying text: String) throws -> Double {
        try lock.withLock {
            guard let (mark, rows) = captured else {
                throw StalledMeasurement(why: "the echo commit was not captured")
            }
            guard rows.contains(where: {
                $0.identifier == "transcript.prompt" && $0.label == text
            }) else {
                throw StalledMeasurement(why: "the marked echo frame did not carry the sent row")
            }
            return mark.sinceProcessStart
        }
    }
}
