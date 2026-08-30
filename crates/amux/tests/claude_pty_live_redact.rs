#[path = "claude_pty_live/redact.rs"]
mod redact;

#[test]
#[ignore = "rewrites an explicitly selected live capture"]
fn rewrite_live_capture_from_environment() {
    let input = std::env::var("AMUX_REDACT_INPUT").expect("AMUX_REDACT_INPUT is required");
    let output = std::env::var("AMUX_REDACT_OUTPUT").expect("AMUX_REDACT_OUTPUT is required");
    let scratch = std::env::var("AMUX_REDACT_SCRATCH").expect("AMUX_REDACT_SCRATCH is required");
    let raw = std::fs::read_to_string(input).unwrap();
    let redacted = redact::redact(&raw, std::path::Path::new(&scratch)).unwrap();
    std::fs::write(output, redacted).unwrap();
}
