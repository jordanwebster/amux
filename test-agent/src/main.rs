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
                writeln!(stdout, "{}", line).unwrap();
                stdout.write_all(&[0x00]).unwrap(); // NUL = "done"
                stdout.flush().unwrap();
            }
            Err(_) => break,
        }
    }
}
