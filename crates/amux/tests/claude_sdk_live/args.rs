pub const USAGE: &str = "usage: claude_sdk_live all | <scenario>...";

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

    args.iter()
        .map(|name| {
            known.iter().position(|known| known == name).ok_or_else(|| {
                format!(
                    "{USAGE}\nunknown Claude SDK live scenario `{name}`; known: {}",
                    known.join(", ")
                )
            })
        })
        .collect()
}
