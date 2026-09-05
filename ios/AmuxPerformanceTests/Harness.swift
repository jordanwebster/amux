import AmuxCore
import AmuxDesign
import AmuxFeatures
import Observation
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
                url: "https://127.0.0.1:1", tls: .system, token: .fixed("measured")),
            logPath: data.appendingPathComponent("perf.log")))
        stores = StoreBundle(account: AccountId("performance"), now: Workloads.now)
        let stores = stores
        pump = Task { @MainActor in
            for await batch in bridge.events { stores.apply(batch) }
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
    func wait(for signpost: Signpost, seconds: Double = 10) async throws {
        let deadline = ContinuousClock.now + .seconds(seconds)
        while Signposts.first(signpost) == nil {
            if ContinuousClock.now > deadline {
                throw XCTSkip("never reached \(signpost.rawValue)")
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

/// The rows a probe list is showing, so appending to the list is one state
/// change the view observes rather than a hierarchy rebuilt by the test.
@MainActor
@Observable
final class ProbeListRows {
    var rows: [ProbeRow]

    init(entries: [FeedEntry]) {
        self.rows = entries.map(ProbeRow.init(entry:))
    }

    func append(_ entries: [FeedEntry]) {
        rows.append(contentsOf: entries.map(ProbeRow.init(entry:)))
    }
}

/// The probe list, reading its rows from the box the run appends to.
struct ProbeList: View {
    let box: ProbeListRows

    var body: some View {
        ProbeListScreen(rows: box.rows)
    }
}
