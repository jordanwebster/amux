import AmuxMobile
import Foundation

// The callback must never run: this build excludes plaintext relay support.
private func unexpectedEvent(_ events: UnsafePointer<CChar>?, _ context: UnsafeMutableRawPointer?) {
    fatalError("Shipping bridge accepted a debug relay")
}

let version = String(cString: amux_mobile_version())
precondition(!version.isEmpty, "Missing bridge version")
print("amux_mobile_version=\(version)")

let root = FileManager.default.temporaryDirectory.appendingPathComponent(UUID().uuidString).path
let config = """
{"data_dir":"\(root)/data","cache_dir":"\(root)/cache","log_path":"\(root)/amux.log",\
"device_name":"linkage-smoke","relay":{"url":"http://127.0.0.1:9",\
"tls":"PlainLoopback","token":{"Static":"unused"}}}
"""
let handle = config.withCString { amux_mobile_start($0, unexpectedEvent, nil) }
if let handle {
    amux_mobile_stop(handle)
    fatalError("Shipping bridge must reject PlainLoopback")
}
print("PlainLoopback rejected by the shipping mobile library")
