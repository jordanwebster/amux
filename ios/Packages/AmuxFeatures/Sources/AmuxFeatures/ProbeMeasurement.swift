import AmuxCore
import AmuxDesign
import SwiftUI

/// Not screens of the app: the two the performance harness measures over
/// before any real screen exists.
///
/// They are deliberately plain — a row is a name and a line of text, a
/// transcript row is its text — so that a number taken over them is a floor.
/// Whatever the designed screens cost, they cost at least this, and a
/// regression here is the list machinery rather than a decoration.
public struct ProbeHomeScreen: View {
    @Environment(\.design) private var design
    private let rows: [AgentRow]

    public init(rows: [AgentRow]) {
        self.rows = rows
    }

    public var body: some View {
        ZStack {
            Ground()
            ScrollView {
                LazyVStack(alignment: .leading, spacing: 0) {
                    ForEach(rows) { row in
                        VStack(alignment: .leading, spacing: 2) {
                            Text(row.name)
                                .designFont(.identifier, design)
                                .foregroundStyle(design.ink.color)
                            Text(row.card.agent.workingOn?.text ?? row.workingDirectory)
                                .designFont(.caption, design)
                                .foregroundStyle(design.inkMuted.color)
                        }
                        .padding(design.metrics.rowPadding)
                        .frame(maxWidth: .infinity, alignment: .leading)
                        .identified("probe.home.row.\(row.id.description)", label: row.name)
                    }
                }
            }
            .padding(.horizontal, design.metrics.gutter)
        }
        .identified("probe.home", value: "\(rows.count)")
    }
}

/// One row of the probe transcript: an identity and the line it draws.
///
/// The text is worked out once, when the row arrives, rather than every time
/// the list is drawn. A row that recomputes itself on each pass measures the
/// recomputation instead of the list.
public struct ProbeRow: Identifiable, Sendable, Equatable {
    public let id: UInt64
    public let text: String

    public init(id: UInt64, text: String) {
        self.id = id
        self.text = text
    }

    public init(entry: FeedEntry) {
        self.id = entry.rowId
        self.text = ProbeRow.text(of: entry)
    }

    /// One line for a row, whatever kind it is. A row this build has no
    /// drawing for still has to take up space and still has to be scrolled
    /// past, so it says what it is rather than being dropped.
    static func text(of entry: FeedEntry) -> String {
        switch entry.entryKind {
        case "message", "prompt":
            entry.row["content"]?.arrayValue?.compactMap { $0["value"]?.stringValue }
                .joined(separator: "\n") ?? ""
        case "tool":
            "\(entry.row["name"]?.stringValue ?? "tool") · "
                + (entry.row["outcome"]?["facts"]?["head"]?.stringValue ?? "")
        default:
            entry.row["rule"]?.stringValue ?? entry.entryKind
        }
    }
}

/// A thousand rows of transcript, drawn as plainly as a row can be drawn, with
/// the tail followed as new rows arrive.
///
/// A `List` rather than a stack in a scroll view: the list reuses the rows
/// that scroll off, which is what a transcript of any length needs and what
/// the streaming budget assumes.
public struct ProbeListScreen: View {
    @Environment(\.design) private var design
    private let rows: [ProbeRow]

    public init(rows: [ProbeRow]) {
        self.rows = rows
    }

    public var body: some View {
        List(rows) { row in
            Text(row.text)
                .designFont(.body, design)
                .foregroundStyle(design.ink.color)
                // One line at a fixed height. A plain row is the floor this
                // harness measures against, and a row whose height depends on
                // its text makes the list measure every row it holds each
                // time one arrives — which is a fact about wrapping, not
                // about streaming.
                .lineLimit(1)
                .frame(height: 22, alignment: .leading)
                .listRowBackground(Color.clear)
                .listRowSeparator(.hidden)
        }
        .listStyle(.plain)
        .scrollContentBackground(.hidden)
        .background { Ground() }
        // The tail is followed by anchoring the scroll there rather than by
        // asking to scroll to the last row on every arrival: an explicit
        // scroll to a row makes the list measure every row above it, which at
        // a thousand rows costs more than drawing the frame does.
        .defaultScrollAnchor(.bottom)
        .identified("probe.list", value: "\(rows.count)")
    }
}
