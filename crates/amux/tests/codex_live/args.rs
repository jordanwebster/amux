pub const USAGE: &str = "usage: codex_live all | <scenario>...";

pub fn select(args: &[String], known: &[&str]) -> Result<Vec<usize>, String> {
    if args.is_empty() {
        return Ok(Vec::new());
    }
    if args.iter().any(|arg| arg == "all") {
        if args.len() != 1 {
            return Err(format!(
                "{USAGE}\n`all` cannot be combined with scenario names"
            ));
        }
        return Ok((0..known.len()).collect());
    }

    // A name that is no scenario is a filter that matched nothing, exactly as
    // libtest treats one: `cargo test --workspace <filter>` hands the filter
    // to every test binary, this opt-in suite included, and it must not turn
    // a workspace-wide filtered run into a failure. The names are echoed so a
    // typo in a deliberate invocation is still visible.
    let (matched, unmatched): (Vec<_>, Vec<_>) = args
        .iter()
        .map(|name| (name, known.iter().position(|known| known == name)))
        .partition(|(_, index)| index.is_some());
    if !unmatched.is_empty() {
        eprintln!(
            "{USAGE}\nno Codex live scenario named {}; known: {}",
            unmatched
                .iter()
                .map(|(name, _)| format!("`{name}`"))
                .collect::<Vec<_>>()
                .join(", "),
            known.join(", ")
        );
    }
    Ok(matched.into_iter().filter_map(|(_, index)| index).collect())
}
