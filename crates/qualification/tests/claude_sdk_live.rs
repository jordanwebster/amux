//! Headless Claude under the real provider: `just live -- claude_sdk all`.
//! See live/mod.rs for what a run judges and reports.

#[cfg(unix)]
mod live;

#[cfg(unix)]
fn main() -> std::process::ExitCode {
    use live::Scenario;
    live::main(live::Driver {
        kind: wire::Kind::ClaudeSdk,
        command: "claude",
        recording: |scenario| match scenario {
            Scenario::Initialize | Scenario::Respond | Scenario::Resume => {
                Some("recorded_text_turn")
            }
            Scenario::Decide => Some("recorded_permission_callback"),
            Scenario::Interrupt => Some("recorded_interrupted"),
            _ => None,
        },
        replay: interpret::replay::<interpret::claude_sdk::ClaudeSdk>,
    })
}

#[cfg(not(unix))]
fn main() {
    println!("live claude_sdk: not_run on this platform");
}
