import Foundation

/// One JSON object per line, over a socket that stays open.
///
/// The app's door speaks this shape.
final class DoorLines {
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
                    throw Failure("the door refused the request: \(error)")
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
