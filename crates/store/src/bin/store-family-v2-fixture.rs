use std::io::{self, BufRead as _, Write as _};
use std::path::PathBuf;

use rusqlite::Connection;
use store::Store;

fn family_row(path: &PathBuf) -> (u32, u64) {
    let connection = Connection::open(path).expect("open fixture database for inspection");
    let (shape, generation): (u32, i64) = connection
        .query_row(
            "SELECT shape,generation FROM family_shape WHERE family='chat'",
            [],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .expect("read chat family row");
    (
        shape,
        u64::try_from(generation).expect("chat generation is non-negative"),
    )
}

fn read_command(expected: &str) {
    let mut command = String::new();
    io::stdin()
        .lock()
        .read_line(&mut command)
        .expect("read controller command");
    assert_eq!(command.trim(), expected);
}

#[tokio::main(flavor = "current_thread")]
async fn main() {
    assert_eq!(
        store::CHAT_SHAPE,
        2,
        "fixture must carry family definition v2"
    );
    let path = PathBuf::from(std::env::args_os().nth(1).expect("store path argument"));
    let store = Store::open(&path)
        .await
        .expect("v2 fixture rebuilds the older family");
    let (shape, generation) = family_row(&path);
    assert_eq!(shape, 2);
    println!("READY shape={shape} generation={generation}");
    io::stdout().flush().expect("flush readiness");

    read_command("CLOSE_FOR_UPGRADE");
    store.close().await;
    println!("CLOSED shape={shape} generation={generation}");
    io::stdout().flush().expect("flush close acknowledgement");

    read_command("CHECK_NEWER");
    let before = family_row(&path);
    assert!(matches!(
        Store::open(&path).await,
        Err(fold::StoreError::UnsupportedFormat)
    ));
    assert_eq!(family_row(&path), before);
    println!(
        "CHECKED reopen=UnsupportedFormat shape={} generation={} unchanged=true",
        before.0, before.1
    );
    io::stdout().flush().expect("flush result");
}
