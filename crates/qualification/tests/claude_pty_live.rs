//! Claude in a terminal under the real provider: `just live -- claude_pty all`.
//! See live/mod.rs for what a run judges and reports.

#[cfg(unix)]
mod live;

#[cfg(unix)]
fn main() -> std::process::ExitCode {
    use live::Scenario;
    live::main(live::Driver {
        kind: wire::Kind::ClaudePty,
        command: "claude",
        recording: |scenario| match scenario {
            Scenario::Initialize | Scenario::Respond | Scenario::Resume => "recorded_prompt",
            Scenario::Decide => "recorded_permission_allow_once",
            Scenario::Interrupt => "recorded_interrupt",
        },
        replay: interpret::replay::<interpret::claude_pty::ClaudePty>,
    })
}

#[cfg(not(unix))]
fn main() {
    println!("live claude_pty: not_run on this platform");
}
