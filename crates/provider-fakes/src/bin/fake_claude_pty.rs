//! The fake `claude` for a terminal. Like the real one it also runs
//! headless when started with stream-json input, which is how a host asks
//! what Claude offers.

fn main() {
    let headless = std::env::args().any(|arg| arg == "--input-format");
    std::process::exit(if headless {
        provider_fakes::sdk::main()
    } else {
        provider_fakes::pty::main()
    });
}
