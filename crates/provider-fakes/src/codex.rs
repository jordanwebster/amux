//! `fake-codex`.

use tokio::io::BufReader;

use crate::{DRIFT_EXIT, Mode};

pub fn main() -> i32 {
    let mode = match crate::mode_from_env() {
        Ok(mode) => mode,
        Err(error) => {
            eprintln!("fake-codex: {error}");
            return DRIFT_EXIT;
        }
    };
    let runtime = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
        .expect("a tokio runtime");
    runtime.block_on(async move {
        match mode {
            Mode::Playback(process) => {
                match crate::lines::play(
                    &process,
                    BufReader::new(tokio::io::stdin()),
                    tokio::io::stdout(),
                )
                .await
                {
                    Ok(()) => 0,
                    Err(error) => {
                        eprintln!("fake-codex: {error}");
                        DRIFT_EXIT
                    }
                }
            }
            Mode::Script(_script) => 0,
        }
    })
}
