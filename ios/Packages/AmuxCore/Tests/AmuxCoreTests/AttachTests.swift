import Foundation
import XCTest
@testable import AmuxCore

/// Attaching a photograph and a file: what leaves the phone, and what stands
/// in the message afterwards.
///
/// A token names an artifact somebody else has to be able to fetch, so the
/// order matters and is pinned here: the bytes go first, and the token appears
/// only on the host's own word that they arrived.
@MainActor
final class AttachTests: XCTestCase {
    private let agent = AgentId(UUID(uuidString: "00000000-0000-0000-0000-0000000000A7")!)

    private func bundle(recording stored: @escaping (PickedAttachment, Data) -> Void)
        -> (StoreBundle, () -> OpId?)
    {
        let bundle = StoreBundle(account: AccountId("test"))
        var last: OpId?
        bundle.store = { picked, bytes in
            stored(picked, bytes)
            let op = OpId(UUID())
            last = op
            return op
        }
        return (bundle, { last })
    }

    func testPickedBytesLeaveThePhoneAndNoTokenAppearsUntilTheyAreStored() throws {
        var sent: [(PickedAttachment, Data)] = []
        let (bundle, op) = bundle { sent.append(($0, $1)) }
        let store = bundle.conversation(agent)
        store.draft.insert(text: "Same failure as ")

        let pixels = Data("\u{89}PNG and then some pixels".utf8)
        XCTAssertTrue(bundle.attach(
            PickedAttachment(
                agent: agent, kind: .image, name: "reconnect-loop.png", mime: "image/png"),
            bytes: pixels))
        XCTAssertEqual(sent.count, 1)
        XCTAssertEqual(sent[0].0.name, "reconnect-loop.png")
        XCTAssertEqual(sent[0].1, pixels, "the picked bytes leave unaltered")
        XCTAssertEqual(
            store.draft.ordered, [], "nothing stands in the message until the host has it")

        let photo = DraftAttachment(
            id: ArtifactId("sha256:" + String(repeating: "a1b2c3d4", count: 8)),
            kind: .image, name: "reconnect-loop.png", mime: "image/png",
            size: UInt64(pixels.count))
        store.apply(.opResult(OpResult(op: try XCTUnwrap(op()), outcome: .attachmentStored(photo))))
        XCTAssertEqual(store.draft.ordered.map(\.kind), [.photo])
        XCTAssertEqual(store.draft.ordered.map(\.label), ["reconnect-loop.png"])
        XCTAssertEqual(store.draft.ordered.first?.attachment, photo)

        // The caret stayed after the token, so the sentence goes on where the
        // person left it rather than in front of what they just attached.
        store.draft.insert(text: ", trace in ")
        let trace = Data(#"{"relay":"unreachable"}"#.utf8)
        XCTAssertTrue(bundle.attach(
            PickedAttachment(
                agent: agent, kind: .file, name: "relay-trace.json", mime: "application/json"),
            bytes: trace))
        let file = DraftAttachment(
            id: ArtifactId("sha256:" + String(repeating: "9f8e7d6c", count: 8)),
            kind: .file, name: "relay-trace.json", mime: "application/json",
            size: UInt64(trace.count))
        store.apply(.opResult(OpResult(op: try XCTUnwrap(op()), outcome: .attachmentStored(file))))
        XCTAssertEqual(store.draft.ordered.map(\.kind), [.photo, .file])

        // Both tokens are the shared library's spelling, and the message text
        // carries the two elements it reads back as.
        let elements = Bridge.attachments(in: store.draft.text)
        XCTAssertEqual(
            elements.compactMap(\.mention?.name), ["reconnect-loop.png", "relay-trace.json"])
        XCTAssertEqual(store.draft.attachments, [photo, file])
    }

    func testNothingIsStoredWithoutAConnectionOrWithoutBytes() {
        let unconnected = StoreBundle(account: AccountId("test"))
        XCTAssertFalse(
            unconnected.attach(
                PickedAttachment(agent: agent, kind: .file, name: "a.txt", mime: "text/plain"),
                bytes: Data("x".utf8)),
            "a bundle with no connection behind it stores nothing")
        let (connected, _) = bundle(recording: { _, _ in XCTFail("an empty file left the phone") })
        XCTAssertFalse(
            connected.attach(
                PickedAttachment(agent: agent, kind: .file, name: "empty", mime: "text/plain"),
                bytes: Data()))
    }
}
