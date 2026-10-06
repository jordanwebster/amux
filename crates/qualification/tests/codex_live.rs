//! Codex under the real provider: `just live codex all`, or one scenario
//! such as `just live codex attach`.
//! See live/mod.rs for what a run judges and reports.

#[cfg(unix)]
mod live;

#[cfg(unix)]
fn main() -> std::process::ExitCode {
    use live::Scenario;
    live::main(live::Driver {
        kind: wire::Kind::Codex,
        command: "codex",
        recording: |scenario| match scenario {
            Scenario::Initialize => "recorded_initialize_and_start",
            Scenario::Respond | Scenario::Resume => "recorded_turn_round_trip",
            // The app's prompt leads to the same approval, answered from amux.
            Scenario::Decide | Scenario::Attach => "recorded_approval_allow",
            Scenario::Interrupt => "recorded_interrupt",
        },
        replay: interpret::replay::<interpret::codex::Codex>,
    })
}

#[cfg(not(unix))]
fn main() {
    println!("live codex: not_run on this platform");
}
