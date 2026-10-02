//! The fake `claude`. A host has one `claude` command for both ways amux
//! runs it, as a real machine does: headless with stream-json, or in its own
//! terminal. Started without stream-json input, it plays the terminal.

fn main() {
    let headless = std::env::args().any(|arg| arg == "--input-format");
    std::process::exit(if headless {
        provider_fakes::sdk::main()
    } else {
        provider_fakes::pty::main()
    });
}
