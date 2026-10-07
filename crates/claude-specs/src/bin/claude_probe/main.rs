#[cfg(unix)]
mod capture;
mod probe;

fn main() -> Result<(), Box<dyn std::error::Error>> {
    // Standing in for `claude` on the PATH, the probe takes itself off the
    // PATH the real provider inherits, so a wrapper that runs the next
    // `claude` it finds reaches the real one.
    if std::env::var_os("CLAUDE_CAPTURE_PROXY").is_some() {
        let path = replay_support::path_past_probe(
            &std::env::current_exe()?,
            std::ffi::OsStr::new("claude"),
            &std::env::var_os("PATH").unwrap_or_default(),
        );
        // SAFETY: no other thread is running yet.
        unsafe { std::env::set_var("PATH", path) };
    }
    probe::main()
}
