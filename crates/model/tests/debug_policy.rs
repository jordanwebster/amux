#[test]
#[ignore = "run only by the declared debug-policy verification recipe"]
fn backtrace_probe() {
    panic!("debug policy backtrace probe");
}
