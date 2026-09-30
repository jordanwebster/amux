//! Writes the phone's performance topology for `testnet serve`. The file
//! is generated at run time rather than committed: its long conversation
//! carries a thousand rows of prose and tool output, which is what the
//! phone is measured paging through, and that is nothing to review by eye.

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let Some(path) = std::env::args().nth(1) else {
        return Err("usage: phone-topology PATH".into());
    };
    let mut text = serde_json::to_string_pretty(&qualification::perf::phone::topology())?;
    text.push('\n');
    std::fs::write(&path, text)?;
    println!("wrote {path}");
    Ok(())
}
