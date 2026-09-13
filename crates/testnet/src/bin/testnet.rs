//! The harness as a process: `testnet serve` starts a declared topology and
//! answers control requests on loopback; `testnet script-from-report` turns
//! a recorded report into a provider script.

use clap::Parser;

#[derive(Parser)]
#[command(name = "testnet")]
#[command(about = "Declared daemon topologies with scripted providers, served on loopback")]
struct Cli {
    #[command(subcommand)]
    command: testnet::serve::Command,
}

fn main() {
    if let Err(error) = testnet::serve::run(Cli::parse().command) {
        eprintln!("testnet: {error:#}");
        std::process::exit(1);
    }
}
