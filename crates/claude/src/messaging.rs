//! Claude Code messaging-socket client.

use std::path::Path;

use uuid::Uuid;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct MessageId(pub Uuid);

#[derive(Debug, thiserror::Error)]
pub enum MessagingError {
    #[error("Claude messaging socket I/O failed: {0}")]
    Io(#[from] std::io::Error),
    #[error("Claude messaging sockets require Unix")]
    Unsupported,
}

pub struct MessagingSocket {
    #[cfg(unix)]
    stream: tokio::net::UnixStream,
}

impl MessagingSocket {
    #[cfg(unix)]
    pub async fn connect(path: &Path, token: &str) -> Result<Self, MessagingError> {
        use tokio::io::AsyncWriteExt;

        let mut stream = tokio::net::UnixStream::connect(path).await?;
        let auth = serde_json::json!({"type":"auth","token":token});
        stream.write_all(auth.to_string().as_bytes()).await?;
        stream.write_all(b"\n").await?;
        Ok(Self { stream })
    }

    #[cfg(not(unix))]
    pub async fn connect(_path: &Path, _token: &str) -> Result<Self, MessagingError> {
        Err(MessagingError::Unsupported)
    }

    #[cfg(unix)]
    pub async fn send(&mut self, text: &str) -> Result<MessageId, MessagingError> {
        use tokio::io::AsyncWriteExt;

        let id = MessageId(Uuid::new_v4());
        let message = serde_json::json!({
            "type":"user",
            "message":{"role":"user","content":text},
        });
        self.stream
            .write_all(message.to_string().as_bytes())
            .await?;
        self.stream.write_all(b"\n").await?;
        self.stream.shutdown().await?;
        Ok(id)
    }

    #[cfg(not(unix))]
    pub async fn send(&mut self, _text: &str) -> Result<MessageId, MessagingError> {
        Err(MessagingError::Unsupported)
    }
}

#[cfg(all(test, unix))]
mod tests {
    use tokio::io::AsyncBufReadExt;
    use tokio::net::UnixListener;

    use super::*;

    #[tokio::test]
    async fn sends_auth_then_user_message() {
        let dir = tempfile::Builder::new()
            .prefix("cm")
            .tempdir_in("/tmp")
            .unwrap();
        let path = dir.path().join("message.sock");
        let listener = UnixListener::bind(&path).unwrap();
        let server = tokio::spawn(async move {
            let (stream, _) = listener.accept().await.unwrap();
            let mut lines = tokio::io::BufReader::new(stream).lines();
            let auth: serde_json::Value =
                serde_json::from_str(&lines.next_line().await.unwrap().unwrap()).unwrap();
            let message: serde_json::Value =
                serde_json::from_str(&lines.next_line().await.unwrap().unwrap()).unwrap();
            (auth, message)
        });
        let mut socket = MessagingSocket::connect(&path, "secret").await.unwrap();
        socket.send("hello").await.unwrap();
        let (auth, message) = server.await.unwrap();
        assert_eq!(auth, serde_json::json!({"type":"auth","token":"secret"}));
        assert_eq!(message["message"]["content"], "hello");
    }
}
