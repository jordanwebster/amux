use std::io::{self, Write};

fn main() {
    let stdin = io::stdin();
    let mut stdout = io::stdout();
    let mut buf = String::new();

    loop {
        buf.clear();
        match stdin.read_line(&mut buf) {
            Ok(0) => break, // EOF
            Ok(_) => {
                let line = buf.trim_end_matches('\n').trim_end_matches('\r');
                if line == "pid" {
                    writeln!(stdout, "pid: {}", std::process::id()).unwrap();
                    stdout.flush().unwrap();
                    continue;
                }
                if line == "exit" {
                    writeln!(stdout, "echo: {}", line).unwrap();
                    stdout.flush().unwrap();
                    break;
                }
                // Prefix with "echo: " to distinguish from PTY echo
                writeln!(stdout, "echo: {}", line).unwrap();
                stdout.flush().unwrap();
            }
            Err(_) => break,
        }
    }
}
