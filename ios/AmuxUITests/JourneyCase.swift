import Foundation
import XCTest

/// What every journey driven by a finger needs: the sockets, the ways of
/// reading a screen by the names it declares, and somewhere to leave what it
/// found.
///
/// A journey is a UI test when what it claims is about pressing things.
/// XCUITest is the only accessibility client an app cannot be for itself, so
/// the taps live here; everything the far side does is said over the runner's
/// control channel, and the few things a finger cannot yet do — sending a
/// message, asking a host for its changes — go through the app's own door.
/// The Mac-side journey passes all three addresses in and collects what this
/// leaves in its container.
class JourneyCase: XCTestCase {
    /// How long to wait for something that crosses a relay. Generous, because
    /// every claim here travels to a real host and back.
    let waiting: TimeInterval = 60

    /// What this test found, written out for the journey to assert on and for
    /// a person to read after a failure.
    var record: [String: Any] = [:]

    /// Where the photographs and the record are left for the journey to
    /// collect out of this process's container.
    static func inContainer(_ name: String) -> URL {
        URL(fileURLWithPath: NSTemporaryDirectory()).appendingPathComponent(name)
    }

    /// What the journey told this test to talk to.
    struct Runner {
        let relay: String
        let token: String
        let user: String
        let control: String
        let doorPort: String
        let agent: String
        let host: String

        init() throws {
            let environment = ProcessInfo.processInfo.environment
            func required(_ name: String) throws -> String {
                try XCTUnwrap(environment[name], "the journey did not pass \(name)")
            }
            relay = try required("AMUX_RELAY")
            token = try required("AMUX_TOKEN")
            user = try required("AMUX_USER")
            control = try required("AMUX_CONTROL")
            doorPort = try required("AMUX_DOOR_PORT")
            agent = try required("AMUX_AGENT")
            host = try required("AMUX_HOST")
        }
    }

    /// The app, launched already told what to connect to, so everything on
    /// screen afterwards arrived over a real relay.
    ///
    /// The launch trusts no machine. A phone is admitted by a machine, never
    /// the other way round, so a journey pairs after the launch — with
    /// `pairThroughTheDoor`, or by whatever the journey is itself about.
    ///
    /// - Parameters:
    ///   - signedIn: whether the launch carries an account's relay credential.
    ///     A launch without one is the app as somebody who has not signed in
    ///     opens it, which is the only way to reach what the app does with a
    ///     link that lands before there is an account to put it to.
    ///   - link: a link the launch was opened with, handed over before the
    ///     first frame exactly as the system hands one over.
    ///   - user: which account the credential belongs to, where a test signs
    ///     in as more than one.
    ///   - scripted: whether the account service and the App Store the app
    ///     itself holds are the scripted ones. A launch that signs somebody in
    ///     by pressing the button needs them: a browser at amux.sh has a
    ///     password in it and the App Store's sheet belongs to another
    ///     process, and neither can be driven from here.
    func launch(
        _ runner: Runner, signedIn: Bool = true, link: String? = nil,
        as user: String? = nil, token: String? = nil, scripted: Bool = false
    ) -> XCUIApplication {
        let app = XCUIApplication()
        let credential = signedIn ? [
            "-amux-relay", runner.relay,
            "-amux-token", token ?? runner.token,
            "-amux-user", user ?? runner.user,
        ] : []
        app.launchArguments = ["-amux-door-port", runner.doorPort]
            + credential
            + (scripted ? ["-amux-scripted-cloud"] : [])
            + (link.map { ["-amux-link", $0] } ?? [])
        app.launch()
        return app
    }

    /// Trusts the machine the journey named, by the code that machine
    /// printed.
    ///
    /// A phone nobody has admitted is disowned by every machine on the relay,
    /// so its fleet comes back empty and there is nothing to open. The screens
    /// that read a code belong to the journey about pairing; every other
    /// journey takes the same two steps against the same machine through the
    /// app's own door, which leaves behind the trust the product's own path
    /// writes.
    func pairThroughTheDoor(_ runner: Runner) throws {
        let environment = ProcessInfo.processInfo.environment
        let machine = try XCTUnwrap(
            environment["AMUX_HOST_ID"], "the journey did not pass AMUX_HOST_ID")
        let code = try XCTUnwrap(environment["AMUX_PIN"], "the journey did not pass AMUX_PIN")
        try door(runner, .init(kind: "pairByCode", host: machine, pin: code))
    }

    // MARK: - Talking to the runner and to the app

    /// One JSON object per line, over a socket that stays open.
    ///
    /// The runner's control channel and the app's door speak the same shape,
    /// so one client serves both.
    final class Lines {
        private let input: InputStream
        private let output: OutputStream
        private var pending = Data()

        init(address: String) throws {
            let parts = address.split(separator: ":")
            guard parts.count == 2, let port = UInt32(parts[1]) else {
                throw Failure("\(address) is not host:port")
            }
            var readable: InputStream?
            var writable: OutputStream?
            Stream.getStreamsToHost(
                withName: String(parts[0]), port: Int(port), inputStream: &readable,
                outputStream: &writable)
            guard let readable, let writable else { throw Failure("nothing answered at \(address)") }
            input = readable
            output = writable
            input.open()
            output.open()
        }

        deinit {
            input.close()
            output.close()
        }

        /// Says one request and does not wait for it to finish. For work that
        /// has to still be going when the next thing happens.
        func say(_ request: Any) throws {
            try write(request)
        }

        /// Reads the answer to something said earlier.
        @discardableResult
        func awaitAnswer() throws -> [String: Any] {
            try read()
        }

        /// Says one request and reads the one line that answers it.
        @discardableResult
        func ask(_ request: Any) throws -> [String: Any] {
            try write(request)
            return try read()
        }

        private func write(_ request: Any) throws {
            var line = try JSONSerialization.data(
                withJSONObject: request, options: [.fragmentsAllowed])
            line.append(0x0A)
            try line.withUnsafeBytes { bytes in
                var written = 0
                while written < line.count {
                    let wrote = output.write(
                        bytes.baseAddress!.advanced(by: written).assumingMemoryBound(to: UInt8.self),
                        maxLength: line.count - written)
                    guard wrote > 0 else { throw Failure("the socket stopped taking bytes") }
                    written += wrote
                }
            }
        }

        private func read() throws -> [String: Any] {
            var buffer = [UInt8](repeating: 0, count: 65536)
            let deadline = Date().addingTimeInterval(120)
            while Date() < deadline {
                if let newline = pending.firstIndex(of: 0x0A) {
                    let line = pending[pending.startIndex..<newline]
                    pending = pending[pending.index(after: newline)...]
                    guard !line.isEmpty else { continue }
                    guard let object = try JSONSerialization.jsonObject(with: Data(line))
                        as? [String: Any]
                    else { throw Failure("the answer was not one JSON object") }
                    if let error = object["Error"] {
                        throw Failure("the runner refused the request: \(error)")
                    }
                    return object
                }
                // A stream that has not finished connecting answers a read
                // with -1 and no error. Only a stream that has actually failed
                // or ended is a failure; everything else is waiting.
                let read = input.hasBytesAvailable
                    ? input.read(&buffer, maxLength: buffer.count) : 0
                if read > 0 {
                    pending.append(contentsOf: buffer[0..<read])
                } else if input.streamStatus == .error || input.streamStatus == .atEnd {
                    throw Failure(
                        "the socket \(input.streamStatus == .atEnd ? "closed" : "failed") while "
                        + "an answer was outstanding: "
                        + "\(input.streamError.map(String.init(describing:)) ?? "no reason given")")
                } else {
                    RunLoop.current.run(until: Date().addingTimeInterval(0.05))
                }
            }
            throw Failure("nothing answered within two minutes")
        }

        struct Failure: Error, CustomStringConvertible {
            let description: String
            init(_ description: String) { self.description = description }
        }
    }

    /// One request for the app's own door, whose fields are the door's.
    struct DoorRequest {
        var kind: String
        var agent: String?
        var text: String?
        var prose: String?
        var base: String?
        var seconds: Double?
        var path: String?
        var identifier: String?
        var host: String?
        var pin: String?
        var qr: String?
        /// What a launch that did not sign in signs in with: the same three
        /// the launch itself would have carried.
        var relay: String?
        var token: String?
        var user: String?
        var attachment: String?
        var name: String?
        var mime: String?
        var base64: String?
        var from: Int?
        var to: Int?
        /// What the scripted account service and the scripted App Store will
        /// answer from here on, in the door's own words. A request says only
        /// what it is changing.
        var cloud: [String: Any]?
        var store: [String: Any]?
        /// Which account a request is about, where more than one is signed in.
        var account: String?

        var body: [String: Any] {
            var fields: [String: Any] = ["kind": kind]
            if let agent { fields["agent"] = agent }
            if let text { fields["text"] = text }
            if let prose { fields["prose"] = prose }
            if let base { fields["base"] = base }
            if let seconds { fields["seconds"] = seconds }
            if let path { fields["path"] = path }
            if let identifier { fields["identifier"] = identifier }
            if let host { fields["host"] = host }
            if let pin { fields["pin"] = pin }
            if let qr { fields["qr"] = qr }
            if let relay { fields["relay"] = relay }
            if let token { fields["token"] = token }
            if let user { fields["user"] = user }
            if let attachment { fields["attachment"] = attachment }
            if let name { fields["name"] = name }
            if let mime { fields["mime"] = mime }
            if let base64 { fields["base64"] = base64 }
            if let from { fields["from"] = from }
            if let to { fields["to"] = to }
            if let cloud { fields["cloud"] = cloud }
            if let store { fields["store"] = store }
            if let account { fields["account"] = account }
            return fields
        }
    }

    /// Opens the door, says one thing, and closes it. One connection at a time
    /// is all the door serves, and holding one open across a test would stop
    /// anything else reaching it.
    @discardableResult
    func door(_ runner: Runner, _ request: DoorRequest) throws -> [String: Any] {
        let door = try Lines(address: "127.0.0.1:\(runner.doorPort)")
        let answer = try door.ask(request.body)
        if let complaint = answer["message"] as? String, answer["kind"] as? String == "error" {
            XCTFail("the door refused \(request.kind): \(complaint)")
        }
        return answer
    }

    /// One thing on screen, as the screen itself declared it.
    struct Said {
        let identifier: String
        let label: String
        let value: String
    }

    /// Everything the screen has declared about itself, in the order it draws
    /// it.
    ///
    /// A name declared on a screen reaches the system's accessibility tree as
    /// an identifier and nothing else — the label and the value beside it are
    /// reported up the view tree instead, for the app's own door — so what a
    /// screen says about itself is asked of the door rather than read off the
    /// tree. What can be pressed stays XCUITest's, which is the only client
    /// that can press anything.
    ///
    /// The screen is let settle first: a query taken mid-animation reads a
    /// frame nobody was looking at.
    /// - Parameter settling: whether to let the screen come to rest first.
    ///   A poll that reads the same name every quarter second should not:
    ///   settling renders the whole window until two frames agree, and a
    ///   caret blinking in a field means they never do, so every read would
    ///   cost the full wait. Anything recorded or asserted on should.
    func declared(_ runner: Runner, settling: Bool = true) throws -> [Said] {
        if settling { try door(runner, .init(kind: "settle")) }
        let answer = try door(runner, .init(kind: "query"))
        let elements = (answer["state"] as? [String: Any])?["elements"] as? [[String: Any]] ?? []
        return elements.compactMap { element in
            guard let identifier = element["identifier"] as? String else { return nil }
            return Said(
                identifier: identifier,
                label: element["label"] as? String ?? "",
                value: element["value"] as? String ?? "")
        }
    }

    /// What one named thing says, or nothing where the screen never named it.
    ///
    /// A name lands on more than one element and only one of them carries
    /// what the screen set, so the first that says anything is the answer.
    func said(_ said: [Said], _ identifier: String) -> Said? {
        let matching = said.filter { $0.identifier == identifier }
        return matching.first { !$0.label.isEmpty || !$0.value.isEmpty } ?? matching.first
    }

    /// Waits until a named thing on screen says what it is expected to say,
    /// and answers with whatever it said in the end.
    func waitForValue(_ runner: Runner, _ identifier: String, _ expected: String) throws -> String {
        var seen = ""
        for _ in 0..<10 {
            seen = said(try declared(runner), identifier)?.value ?? ""
            if seen == expected { return seen }
            RunLoop.current.run(until: Date().addingTimeInterval(0.3))
        }
        return seen
    }

    // MARK: - Reading a screen by the names it declares

    func element(_ app: XCUIApplication, _ identifier: String) -> XCUIElement {
        app.descendants(matching: .any).matching(identifier: identifier).firstMatch
    }

    /// What a named element says its value is.
    ///
    /// A name lands on more than one element in the tree the system builds,
    /// and only one of them carries the value the screen set; the others carry
    /// an empty string. So the first one with something in it is the answer.
    func value(_ app: XCUIApplication, _ identifier: String) -> String? {
        app.descendants(matching: .any).matching(identifier: identifier)
            .allElementsBoundByIndex.compactMap { $0.value as? String }
            .first { !$0.isEmpty }
    }

    /// What a named element is called, which is what VoiceOver would read.
    func label(_ app: XCUIApplication, _ identifier: String) -> String? {
        app.descendants(matching: .any).matching(identifier: identifier)
            .allElementsBoundByIndex.map { $0.label }.first { !$0.isEmpty }
    }

    func identifiers(_ app: XCUIApplication, startingWith prefix: String) -> [String] {
        let matching = app.descendants(matching: .any)
            .matching(NSPredicate(format: "identifier BEGINSWITH %@", prefix))
        var seen: [String] = []
        for element in matching.allElementsBoundByIndex where !seen.contains(element.identifier) {
            seen.append(element.identifier)
        }
        return seen
    }

    func transcriptRows(_ app: XCUIApplication) -> [String] {
        identifiers(app, startingWith: "transcript.")
    }

    @discardableResult
    func waitUntil(within seconds: TimeInterval? = nil, _ condition: () -> Bool) -> Bool {
        let deadline = Date().addingTimeInterval(seconds ?? waiting)
        while Date() < deadline {
            if condition() { return true }
            RunLoop.current.run(until: Date().addingTimeInterval(0.25))
        }
        return condition()
    }

    /// Waits for a named element to be on screen, and says what was there
    /// instead when it never arrives.
    func waitFor(_ app: XCUIApplication, _ identifier: String, _ complaint: String) {
        // What is reported instead is everything under the same first word:
        // the whole tree of a long transcript is unreadable in a failure, and
        // the neighbours of the missing name are what says what went wrong.
        let family = identifier.split(separator: ".").first.map(String.init) ?? identifier
        XCTAssertTrue(
            element(app, identifier).waitForExistence(timeout: waiting),
            "\(complaint); the screen shows \(identifiers(app, startingWith: family))")
    }

    func waitForNo(_ app: XCUIApplication, _ identifier: String, _ complaint: String) {
        XCTAssertTrue(
            waitUntil { !self.element(app, identifier).exists },
            "\(complaint); \(identifier) is still on screen")
    }

    /// Answers the ask on screen, and answers again if it is still waiting.
    ///
    /// An ask reaches the phone the moment the agent raises it and the
    /// session keeps moving underneath: an answer sent in that window is
    /// refused by the host on purpose — it raced a session that had gone on
    /// — and the core puts the ask back rather than leaving it looking
    /// answered. A person presses again, and so does this. Nothing here
    /// claims the answer arrived: what the host received is read back from
    /// the host afterwards, so a press that went nowhere can never be
    /// mistaken for one that did.
    func answer(_ app: XCUIApplication, _ identifier: String, _ complaint: String) {
        for _ in 0..<3 {
            press(app, identifier)
            guard waitUntil(within: 15, { !self.element(app, identifier).exists }) else { continue }
            // Gone is not the same as answered: while the answer travels the
            // panel is put away, and a refusal brings the ask back. So the
            // panel has to stay away before this counts as answered.
            if !waitUntil(within: 3, { self.element(app, identifier).exists }) { return }
        }
        XCTFail("\(complaint); \(identifier) is still on screen")
    }

    func pressTab(_ app: XCUIApplication, _ title: String) {
        let button = app.tabBars.buttons[title]
        guard button.waitForExistence(timeout: waiting) else {
            return XCTFail("the tab bar has no \(title) tab")
        }
        button.tap()
    }

    /// Presses the named control, once it is somewhere a finger could land.
    ///
    /// A control that has just arrived exists before it can be touched: a
    /// panel sliding in reports its buttons the whole way, and a tap sent
    /// while one is still moving lands where the button is about to be and is
    /// swallowed. So a moment is given for something hittable to settle —
    /// briefly, because plenty of things worth pressing sit under the
    /// floating chrome and are only ever reachable by coordinate.
    func press(_ app: XCUIApplication, _ identifier: String) {
        var candidates: [XCUIElement] = []
        waitUntil(within: 5) {
            candidates = app.descendants(matching: .any)
                .matching(identifier: identifier).allElementsBoundByIndex
            return candidates.contains { $0.isHittable }
        }
        let hittable = candidates.first(where: { $0.isHittable })
        guard let target = hittable ?? candidates.first else {
            return XCTFail("nothing on screen is named \(identifier)")
        }
        if target.isHittable {
            target.tap()
        } else {
            target.coordinate(withNormalizedOffset: CGVector(dx: 0.5, dy: 0.5)).tap()
        }
    }

    /// Scrolls until the named row is somewhere a finger could reach it.
    func reveal(_ app: XCUIApplication, _ identifier: String) {
        for attempt in 0..<40 {
            // On screen is not the same as reachable: a row under the floating
            // chrome or behind the panel at the foot exists and cannot be
            // pressed, and scrolling one more step is what a person does about
            // that.
            let candidates = app.descendants(matching: .any)
                .matching(identifier: identifier).allElementsBoundByIndex
            if candidates.contains(where: { $0.isHittable }) { return }
            if attempt < 20 { app.swipeDown(velocity: .slow) } else { app.swipeUp(velocity: .slow) }
        }
        XCTFail("scrolling never brought \(identifier) somewhere it could be pressed")
    }

    func photograph(_ app: XCUIApplication, _ name: String) {
        let shot = XCUIScreen.main.screenshot()
        try? shot.pngRepresentation.write(to: Self.inContainer("\(name).png"))
        let attached = XCTAttachment(screenshot: shot)
        attached.name = name
        attached.lifetime = .keepAlways
        add(attached)
    }

    /// Everything this test read, left where the Mac can collect it. The
    /// journey asserts on this as well as on the test passing, so a failure
    /// afterwards can be read against what was actually on screen.
    func write(_ name: String) throws {
        let data = try JSONSerialization.data(
            withJSONObject: record, options: [.prettyPrinted, .sortedKeys])
        try data.write(to: Self.inContainer(name))
    }
}
