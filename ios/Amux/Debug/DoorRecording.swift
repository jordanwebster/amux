import AmuxCore
import AmuxMobile
import Foundation

/// Writing what this app has been through into a bundle, and rebuilding a
/// screen from one somebody else wrote.
///
/// A bug on a phone is reported as the picture of the screen it was found on
/// and the recordings behind it, in one directory, with `report.json`
/// declaring every part and the reason for each one that is missing.
/// `msgs.jsonl` is the shared runtime's own recording: the reducer model it
/// had checkpointed and every message it folded after that. `trace.jsonl` is
/// what the person was looking at while those messages arrived. Rebuilding
/// means folding the first back into a model, projecting that model into the
/// same events a live connection would have delivered, and then applying the
/// second — so the screen comes back without the phone, the relay, the host
/// or any of the work the recording originally asked for.
///
/// The bundle is assembled by the same code the Send button uses, so what a
/// driver collects is what a person's report would have been rather than a
/// second layout that could drift from it.
///
/// This is a driving tool. The calls it makes exist only in the library built
/// with the driving tools compiled in, so it must never be reachable from a
/// build a person could install.
enum DoorRecording {
    enum Failure: Error, CustomStringConvertible {
        case notPhotographed
        case unreadable(String)
        case refused(String)

        var description: String {
            switch self {
            case .notPhotographed: "the screen could not be photographed, so there is no report"
            case .unreadable(let what): "the bundle's \(what) could not be read"
            case .refused(let why): "the recording could not be replayed: \(why)"
            }
        }
    }

    /// Freezes the screen the way a screenshot does, assembles the report this
    /// app would send about it, writes every part it has into a directory and
    /// answers the files it left there.
    ///
    /// A part the phone could not take is not written, and is not silently
    /// dropped either: `report.json` says it is absent and why, which is the
    /// thing a reader of the bundle needs and a directory listing cannot say.
    @MainActor
    static func write(
        _ directory: URL,
        freezer: any ReportFreezing,
        draft: ReportDraft,
        build: String,
        log: Result<String, PartAbsent>
    ) throws -> [String] {
        let report = ReportStore()
        guard report.begin(freezer) else { throw Failure.notPhotographed }
        report.draft = draft
        guard let bundle = report.assembled(build: build, gitSHA: AppFiles.gitSHA, log: log) else {
            throw Failure.notPhotographed
        }

        try FileManager.default.createDirectory(
            at: directory, withIntermediateDirectories: true)
        var written: [String] = []
        for part in bundle.parts {
            guard let data = part.data else { continue }
            try data.write(to: directory.appendingPathComponent(part.name))
            written.append(part.name)
        }
        return written
    }

    /// Folds a bundle's runtime recording into fresh stores.
    ///
    /// Nothing is started and nothing is sent: the shared reducer folds the
    /// recorded messages and its projection is read, which is the same read
    /// surface a live connection delivers. The effects the recording once
    /// asked for are not carried out — they were carried out on the phone
    /// that wrote it.
    @MainActor
    static func replay(_ directory: URL, into stores: StoreBundle) throws -> [Event] {
        let messages = directory.appendingPathComponent(Trace.messagesFile)
        let json = messages.path.withCString { path -> String? in
            guard let owned = amux_mobile_replay_report(path) else { return nil }
            defer { amux_mobile_free(owned) }
            return String(cString: owned)
        }
        guard let json else { throw Failure.unreadable(Trace.messagesFile) }
        struct Replayed: Decodable {
            var events: [Event]?
            var error: String?
        }
        guard let replayed = try? AmuxJSON.decoder.decode(Replayed.self, from: Data(json.utf8))
        else { throw Failure.unreadable(Trace.messagesFile) }
        if let error = replayed.error { throw Failure.refused(error) }
        guard let events = replayed.events else { throw Failure.unreadable(Trace.messagesFile) }
        stores.apply(events)
        return events
    }

    /// The view-state recording beside it, or nothing when the bundle has none.
    static func trace(_ directory: URL) throws -> [TraceEvent] {
        let path = directory.appendingPathComponent(Trace.traceFile)
        guard let text = try? String(contentsOf: path, encoding: .utf8) else { return [] }
        guard let events = try? Trace.events(text) else { throw Failure.unreadable(Trace.traceFile) }
        return events
    }
}
