//! `claude-probe` is spec-recording tooling and exists only in debug profiles
//! (see this crate's build script). The release build still has to produce a
//! binary for the target, so it gets this stub.
#[cfg(specs)]
mod probe;

#[cfg(specs)]
fn main() -> Result<(), Box<dyn std::error::Error>> {
    probe::main()
}

#[cfg(not(specs))]
fn main() {
    eprintln!("claude-probe is spec tooling and is not built in release profiles");
    std::process::exit(2);
}
