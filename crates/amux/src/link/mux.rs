use std::collections::VecDeque;
use std::future::{self, poll_fn};
use std::io;
use std::pin::Pin;
use std::sync::Mutex;
use std::task::{Context, Poll};

use futures_util::future::BoxFuture;
use tokio::io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt, ReadBuf};
use tokio::sync::{mpsc, oneshot, watch};
use tokio_util::compat::{FuturesAsyncReadCompatExt, TokioAsyncReadCompatExt};

use super::carrier::{
    AsyncStream, BoxIoFuture, ByteStream, CarrierKind, ControlSink, ControlSource, ControlWrite,
    LinkCarrier, OpenError,
};
use crate::protocol::wire::{self, pb};

const STREAM_ACCEPTED: u8 = 0;
const STREAM_REFUSED: u8 = 1;
const CONTROL_QUEUE_CAPACITY: usize = 32;
const CLOSE_GRACE: std::time::Duration = std::time::Duration::from_millis(100);

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum MuxRole {
    Connector,
    Acceptor,
}

#[derive(Debug)]
enum DriverOpenError {
    Closed,
    Io(String),
}

enum DriverCommand {
    Open(oneshot::Sender<Result<yamux::Stream, DriverOpenError>>),
    Close,
}

pub(crate) struct MuxCarrier {
    kind: CarrierKind,
    commands: mpsc::UnboundedSender<DriverCommand>,
    driver: tokio::task::AbortHandle,
    inbound: tokio::sync::Mutex<mpsc::UnboundedReceiver<(pb::StreamPreface, ByteStream)>>,
    control: Mutex<Option<(ControlSink, ControlSource)>>,
    control_ready: watch::Receiver<bool>,
    closed: watch::Sender<Option<pb::LinkCloseReason>>,
}

impl MuxCarrier {
    pub(crate) fn new<IO>(io: IO, role: MuxRole, kind: CarrierKind) -> Self
    where
        IO: AsyncRead + AsyncWrite + Send + Unpin + 'static,
    {
        let (commands, command_rx) = mpsc::unbounded_channel();
        let (inbound_tx, inbound) = mpsc::unbounded_channel();
        let (control_stream_tx, control_stream_rx) = oneshot::channel();
        let (closed, _) = watch::channel(None);
        let (control_ready_tx, control_ready) = watch::channel(false);

        let mut first_inbound = match role {
            MuxRole::Connector => {
                commands
                    .send(DriverCommand::Open(control_stream_tx))
                    .expect("new yamux driver must accept its control-stream command");
                None
            }
            MuxRole::Acceptor => Some(control_stream_tx),
        };
        let mode = match role {
            MuxRole::Connector => yamux::Mode::Client,
            MuxRole::Acceptor => yamux::Mode::Server,
        };
        let driver_closed = closed.clone();
        let driver = tokio::spawn(async move {
            drive_connection(
                yamux::Connection::new(io.compat(), yamux::Config::default(), mode),
                command_rx,
                inbound_tx,
                &mut first_inbound,
                driver_closed,
            )
            .await;
        });

        let (control_write_tx, control_write_rx) = mpsc::channel(CONTROL_QUEUE_CAPACITY);
        let (control_read_tx, control_read_rx) = mpsc::channel(CONTROL_QUEUE_CAPACITY);
        tokio::spawn(run_control_stream(
            control_stream_rx,
            control_write_rx,
            control_read_tx,
            control_ready_tx,
        ));

        Self {
            kind,
            commands,
            driver: driver.abort_handle(),
            inbound: tokio::sync::Mutex::new(inbound),
            control: Mutex::new(Some((
                ControlSink {
                    tx: control_write_tx,
                },
                ControlSource {
                    rx: control_read_rx,
                },
            ))),
            control_ready,
            closed,
        }
    }

    async fn next_inbound(&self) -> Option<(pb::StreamPreface, ByteStream)> {
        self.inbound.lock().await.recv().await
    }

    async fn request_stream(&self) -> Result<yamux::Stream, OpenError> {
        let mut ready = self.control_ready.clone();
        while !*ready.borrow_and_update() {
            ready.changed().await.map_err(|_| OpenError::LinkClosed)?;
        }
        let (result, opened) = oneshot::channel();
        self.commands
            .send(DriverCommand::Open(result))
            .map_err(|_| OpenError::LinkClosed)?;
        match opened.await {
            Ok(Ok(stream)) => Ok(stream),
            Ok(Err(DriverOpenError::Closed)) | Err(_) => Err(OpenError::LinkClosed),
            Ok(Err(DriverOpenError::Io(error))) => Err(OpenError::Io(io::Error::other(error))),
        }
    }

    #[cfg(testnet)]
    pub(crate) async fn open_without_preface(&self) -> io::Result<pb::StreamRefusal> {
        let mut stream = self
            .request_stream()
            .await
            .map_err(|error| io::Error::other(error.to_string()))?
            .compat();
        // A zero-length protobuf frame contains no StreamPreface destination.
        stream.write_all(&0_u32.to_be_bytes()).await?;
        stream.flush().await?;
        let status = stream.read_u8().await?;
        if status != STREAM_REFUSED {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                format!("expected stream refusal, got status {status}"),
            ));
        }
        let code = stream.read_u8().await?;
        Ok(pb::StreamRefusal::try_from(i32::from(code)).unwrap_or(pb::StreamRefusal::Unspecified))
    }
}

impl LinkCarrier for MuxCarrier {
    fn kind(&self) -> CarrierKind {
        self.kind
    }

    fn control(&self) -> (ControlSink, ControlSource) {
        self.control
            .lock()
            .expect("control stream lock poisoned")
            .take()
            .expect("a link control stream can only be taken once")
    }

    fn open_stream(
        &self,
        preface: pb::StreamPreface,
    ) -> BoxFuture<'_, Result<ByteStream, OpenError>> {
        Box::pin(async move {
            let mut stream = self.request_stream().await?.compat();
            write_proto(&mut stream, &preface, wire::MESSAGE_SIZE_LIMIT)
                .await
                .map_err(OpenError::Io)?;
            stream.flush().await.map_err(OpenError::Io)?;

            let status = stream.read_u8().await.map_err(OpenError::Io)?;
            match status {
                STREAM_ACCEPTED => Ok(Box::new(MuxByteStream::accepted(stream)) as ByteStream),
                STREAM_REFUSED => {
                    let code = stream.read_u8().await.map_err(OpenError::Io)?;
                    let refusal = pb::StreamRefusal::try_from(i32::from(code))
                        .unwrap_or(pb::StreamRefusal::Unspecified);
                    Err(OpenError::Refused(refusal))
                }
                other => Err(OpenError::Io(io::Error::new(
                    io::ErrorKind::InvalidData,
                    format!("invalid yamux stream-open status {other}"),
                ))),
            }
        })
    }

    fn accept_stream(&self) -> BoxFuture<'_, Option<(pb::StreamPreface, ByteStream)>> {
        Box::pin(self.next_inbound())
    }

    fn close(&self, reason: pb::LinkCloseReason) {
        self.closed.send_replace(Some(reason));
        let _ = self.commands.send(DriverCommand::Close);
        let driver = self.driver.clone();
        tokio::spawn(async move {
            tokio::time::sleep(CLOSE_GRACE).await;
            driver.abort();
        });
    }

    fn closed(&self) -> BoxFuture<'_, pb::LinkCloseReason> {
        Box::pin(async move {
            let mut closed = self.closed.subscribe();
            loop {
                if let Some(reason) = *closed.borrow_and_update() {
                    return reason;
                }
                if closed.changed().await.is_err() {
                    return pb::LinkCloseReason::Unspecified;
                }
            }
        })
    }
}

enum DriverEvent {
    Inbound(yamux::Stream),
    Closed,
}

async fn drive_connection<IO>(
    mut connection: yamux::Connection<tokio_util::compat::Compat<IO>>,
    mut commands: mpsc::UnboundedReceiver<DriverCommand>,
    inbound: mpsc::UnboundedSender<(pb::StreamPreface, ByteStream)>,
    first_inbound: &mut Option<oneshot::Sender<Result<yamux::Stream, DriverOpenError>>>,
    closed: watch::Sender<Option<pb::LinkCloseReason>>,
) where
    IO: AsyncRead + AsyncWrite + Send + Unpin + 'static,
{
    let mut pending_opens = VecDeque::new();
    let mut closing = false;

    loop {
        let event = poll_fn(|cx| {
            for _ in 0..64 {
                match commands.poll_recv(cx) {
                    Poll::Ready(Some(DriverCommand::Open(result))) if !closing => {
                        pending_opens.push_back(result);
                    }
                    Poll::Ready(Some(DriverCommand::Open(result))) => {
                        let _ = result.send(Err(DriverOpenError::Closed));
                    }
                    Poll::Ready(Some(DriverCommand::Close)) | Poll::Ready(None) => {
                        closing = true;
                        break;
                    }
                    Poll::Pending => break,
                }
            }

            if closing {
                return match connection.poll_close(cx) {
                    Poll::Ready(_) => Poll::Ready(DriverEvent::Closed),
                    Poll::Pending => Poll::Pending,
                };
            }

            while let Some(result) = pending_opens.pop_front() {
                match connection.poll_new_outbound(cx) {
                    Poll::Ready(Ok(stream)) => {
                        let _ = result.send(Ok(stream));
                    }
                    Poll::Ready(Err(yamux::ConnectionError::Closed)) => {
                        let _ = result.send(Err(DriverOpenError::Closed));
                    }
                    Poll::Ready(Err(error)) => {
                        let _ = result.send(Err(DriverOpenError::Io(error.to_string())));
                    }
                    Poll::Pending => {
                        pending_opens.push_front(result);
                        break;
                    }
                }
            }

            match connection.poll_next_inbound(cx) {
                Poll::Ready(Some(Ok(stream))) => Poll::Ready(DriverEvent::Inbound(stream)),
                Poll::Ready(Some(Err(_))) | Poll::Ready(None) => Poll::Ready(DriverEvent::Closed),
                Poll::Pending => Poll::Pending,
            }
        })
        .await;

        match event {
            DriverEvent::Inbound(stream) => {
                if let Some(control) = first_inbound.take() {
                    let _ = control.send(Ok(stream));
                } else {
                    let inbound = inbound.clone();
                    tokio::spawn(async move {
                        prepare_inbound_stream(stream, inbound).await;
                    });
                }
            }
            DriverEvent::Closed => break,
        }
    }

    for result in pending_opens {
        let _ = result.send(Err(DriverOpenError::Closed));
    }
    if let Some(control) = first_inbound.take() {
        let _ = control.send(Err(DriverOpenError::Closed));
    }
    if closed.borrow().is_none() {
        closed.send_replace(Some(pb::LinkCloseReason::Unspecified));
    }
}

async fn prepare_inbound_stream(
    stream: yamux::Stream,
    inbound: mpsc::UnboundedSender<(pb::StreamPreface, ByteStream)>,
) {
    let mut stream = stream.compat();
    match read_proto::<_, pb::StreamPreface>(&mut stream, wire::MESSAGE_SIZE_LIMIT).await {
        Ok(Some(preface)) => {
            let stream = Box::new(MuxByteStream::pending(stream)) as ByteStream;
            if let Err(error) = inbound.send((preface, stream)) {
                let (_, mut stream) = error.0;
                let _ = stream.reset(pb::StreamRefusal::ShuttingDown).await;
            }
        }
        Ok(None) | Err(_) => {
            let _ = refuse_stream(&mut stream, pb::StreamRefusal::NotAdjacent).await;
        }
    }
}

async fn run_control_stream(
    stream: oneshot::Receiver<Result<yamux::Stream, DriverOpenError>>,
    writes: mpsc::Receiver<ControlWrite>,
    reads: mpsc::Sender<io::Result<pb::Message>>,
    ready: watch::Sender<bool>,
) {
    let Ok(Ok(stream)) = stream.await else {
        return;
    };
    ready.send_replace(true);
    let (reader, writer) = tokio::io::split(stream.compat());
    let read = read_control_messages(reader, reads);
    let write = write_control_messages(writer, writes);
    tokio::pin!(read);
    tokio::pin!(write);
    tokio::select! {
        _ = &mut read => {}
        _ = &mut write => {}
    }
}

async fn refuse_stream(
    stream: &mut tokio_util::compat::Compat<yamux::Stream>,
    reason: pb::StreamRefusal,
) -> io::Result<()> {
    stream.write_all(&[STREAM_REFUSED, reason as u8]).await?;
    stream.flush().await?;
    stream.shutdown().await
}

async fn read_control_messages<R>(mut reader: R, sender: mpsc::Sender<io::Result<pb::Message>>)
where
    R: AsyncRead + Unpin,
{
    loop {
        match read_proto(&mut reader, wire::MESSAGE_SIZE_LIMIT).await {
            Ok(Some(message)) => {
                if sender.send(Ok(message)).await.is_err() {
                    return;
                }
            }
            Ok(None) => return,
            Err(error) => {
                let _ = sender.send(Err(error)).await;
                future::pending::<()>().await;
            }
        }
    }
}

async fn write_control_messages<W>(mut writer: W, mut receiver: mpsc::Receiver<ControlWrite>)
where
    W: AsyncWrite + Unpin,
{
    while let Some(ControlWrite {
        bytes,
        declared_len,
        result,
    }) = receiver.recv().await
    {
        let write_result = write_frame(&mut writer, &bytes, declared_len).await;
        match write_result {
            Ok(()) => {
                let _ = result.send(Ok(()));
            }
            Err(error) => {
                let kind = error.kind();
                let message = error.to_string();
                let _ = result.send(Err(io::Error::new(kind, message.clone())));
                while let Ok(command) = receiver.try_recv() {
                    let _ = command
                        .result
                        .send(Err(io::Error::new(kind, message.clone())));
                }
                return;
            }
        }
    }
    let _ = writer.shutdown().await;
}

async fn write_frame<W>(writer: &mut W, bytes: &[u8], declared_len: Option<u32>) -> io::Result<()>
where
    W: AsyncWrite + Unpin,
{
    let len = match declared_len {
        Some(len) => len,
        None => u32::try_from(bytes.len())
            .map_err(|_| io::Error::new(io::ErrorKind::InvalidInput, "frame is too large"))?,
    };
    writer.write_all(&len.to_be_bytes()).await?;
    writer.write_all(bytes).await?;
    writer.flush().await
}

async fn write_proto<W, M>(writer: &mut W, message: &M, limit: usize) -> io::Result<()>
where
    W: AsyncWrite + Unpin,
    M: prost::Message,
{
    let encoded_len = message.encoded_len();
    if encoded_len > limit {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            format!("protobuf frame is {encoded_len} bytes; limit is {limit}"),
        ));
    }
    let mut bytes = Vec::with_capacity(encoded_len);
    message
        .encode(&mut bytes)
        .map_err(|error| io::Error::new(io::ErrorKind::InvalidInput, error))?;
    write_frame(writer, &bytes, None).await
}

async fn read_proto<R, M>(reader: &mut R, limit: usize) -> io::Result<Option<M>>
where
    R: AsyncRead + Unpin,
    M: prost::Message + Default,
{
    let mut header = [0_u8; 4];
    match reader.read_exact(&mut header).await {
        Ok(_) => {}
        Err(error) if error.kind() == io::ErrorKind::UnexpectedEof => return Ok(None),
        Err(error) => return Err(error),
    }
    let len = u32::from_be_bytes(header) as usize;
    if len > limit {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            format!("protobuf frame is {len} bytes; limit is {limit}"),
        ));
    }
    let mut bytes = vec![0; len];
    reader.read_exact(&mut bytes).await?;
    M::decode(bytes.as_slice())
        .map(Some)
        .map_err(|error| io::Error::new(io::ErrorKind::InvalidData, error))
}

#[derive(Clone, Copy, Eq, PartialEq)]
enum Admission {
    Pending,
    Accepted,
    Closed,
}

struct MuxByteStream {
    stream: Option<tokio_util::compat::Compat<yamux::Stream>>,
    admission: Admission,
    accept_offset: usize,
    accepting_flush: bool,
}

impl MuxByteStream {
    fn pending(stream: tokio_util::compat::Compat<yamux::Stream>) -> Self {
        Self {
            stream: Some(stream),
            admission: Admission::Pending,
            accept_offset: 0,
            accepting_flush: false,
        }
    }

    fn accepted(stream: tokio_util::compat::Compat<yamux::Stream>) -> Self {
        Self {
            stream: Some(stream),
            admission: Admission::Accepted,
            accept_offset: 0,
            accepting_flush: false,
        }
    }

    fn poll_accept(&mut self, cx: &mut Context<'_>) -> Poll<io::Result<()>> {
        if self.admission != Admission::Pending {
            return Poll::Ready(Ok(()));
        }
        let stream = self.stream.as_mut().ok_or_else(stream_closed_error)?;
        if !self.accepting_flush {
            while self.accept_offset < 1 {
                let written = match Pin::new(&mut *stream)
                    .poll_write(cx, &[STREAM_ACCEPTED][self.accept_offset..])
                {
                    Poll::Ready(result) => result?,
                    Poll::Pending => return Poll::Pending,
                };
                if written == 0 {
                    return Poll::Ready(Err(io::Error::new(
                        io::ErrorKind::WriteZero,
                        "could not accept yamux stream",
                    )));
                }
                self.accept_offset += written;
            }
            self.accepting_flush = true;
        }
        match Pin::new(stream).poll_flush(cx) {
            Poll::Ready(Ok(())) => {
                self.admission = Admission::Accepted;
                Poll::Ready(Ok(()))
            }
            other => other,
        }
    }

    async fn ensure_accepted(&mut self) -> io::Result<()> {
        poll_fn(|cx| self.poll_accept(cx)).await
    }
}

impl AsyncRead for MuxByteStream {
    fn poll_read(
        mut self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: &mut ReadBuf<'_>,
    ) -> Poll<io::Result<()>> {
        match self.poll_accept(cx) {
            Poll::Ready(Ok(())) => {}
            Poll::Ready(Err(error)) => return Poll::Ready(Err(error)),
            Poll::Pending => return Poll::Pending,
        }
        match self.stream.as_mut() {
            Some(stream) => Pin::new(stream).poll_read(cx, buf),
            None => Poll::Ready(Ok(())),
        }
    }
}

impl AsyncWrite for MuxByteStream {
    fn poll_write(
        mut self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: &[u8],
    ) -> Poll<io::Result<usize>> {
        match self.poll_accept(cx) {
            Poll::Ready(Ok(())) => {}
            Poll::Ready(Err(error)) => return Poll::Ready(Err(error)),
            Poll::Pending => return Poll::Pending,
        }
        match self.stream.as_mut() {
            Some(stream) => Pin::new(stream).poll_write(cx, buf),
            None => Poll::Ready(Err(stream_closed_error())),
        }
    }

    fn poll_flush(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<io::Result<()>> {
        match self.poll_accept(cx) {
            Poll::Ready(Ok(())) => {}
            Poll::Ready(Err(error)) => return Poll::Ready(Err(error)),
            Poll::Pending => return Poll::Pending,
        }
        match self.stream.as_mut() {
            Some(stream) => Pin::new(stream).poll_flush(cx),
            None => Poll::Ready(Err(stream_closed_error())),
        }
    }

    fn poll_shutdown(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<io::Result<()>> {
        match self.poll_accept(cx) {
            Poll::Ready(Ok(())) => {}
            Poll::Ready(Err(error)) => return Poll::Ready(Err(error)),
            Poll::Pending => return Poll::Pending,
        }
        match self.stream.as_mut() {
            Some(stream) => Pin::new(stream).poll_shutdown(cx),
            None => Poll::Ready(Ok(())),
        }
    }
}

impl AsyncStream for MuxByteStream {
    fn finish(&mut self) -> BoxFuture<'_, io::Result<()>> {
        Box::pin(async move {
            self.ensure_accepted().await?;
            if let Some(stream) = self.stream.as_mut() {
                stream.shutdown().await?;
            }
            self.admission = Admission::Closed;
            Ok(())
        })
    }

    fn reset(&mut self, code: pb::StreamRefusal) -> BoxIoFuture<'_, ()> {
        Box::pin(async move {
            if self.admission == Admission::Pending
                && let Some(stream) = self.stream.as_mut()
            {
                stream.write_all(&[STREAM_REFUSED, code as u8]).await?;
                stream.flush().await?;
            }
            self.admission = Admission::Closed;
            self.stream.take();
            Ok(())
        })
    }
}

fn stream_closed_error() -> io::Error {
    io::Error::new(io::ErrorKind::BrokenPipe, "yamux stream closed")
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;
    use std::time::Duration;

    use tokio::io::{AsyncReadExt, AsyncWriteExt};

    use super::*;
    use crate::link::carrier::{read_message, write_message};
    use crate::protocol::wire::pb::message;

    fn new_carriers() -> (Arc<MuxCarrier>, Arc<MuxCarrier>) {
        let (connector_io, acceptor_io) = tokio::io::duplex(2 * 1024 * 1024);
        (
            Arc::new(MuxCarrier::new(
                connector_io,
                MuxRole::Connector,
                CarrierKind::RelayTcp,
            )),
            Arc::new(MuxCarrier::new(
                acceptor_io,
                MuxRole::Acceptor,
                CarrierKind::RelayTcp,
            )),
        )
    }

    async fn carriers() -> (Arc<MuxCarrier>, Arc<MuxCarrier>) {
        let (connector, acceptor) = new_carriers();
        let (mut connector_sink, _connector_source) = connector.control();
        let (_acceptor_sink, mut acceptor_source) = acceptor.control();
        write_message(&mut connector_sink, &pb::Message { body: None })
            .await
            .unwrap();
        assert_eq!(
            read_message(&mut acceptor_source).await.unwrap(),
            Some(pb::Message { body: None })
        );
        (connector, acceptor)
    }

    fn preface(dst: u8) -> pb::StreamPreface {
        pb::StreamPreface { dst: vec![dst; 16] }
    }

    async fn open_with_partial_preface(
        carrier: &MuxCarrier,
        preface: &pb::StreamPreface,
    ) -> (tokio_util::compat::Compat<yamux::Stream>, Vec<u8>) {
        let mut stream = carrier.request_stream().await.unwrap().compat();
        let bytes = prost::Message::encode_to_vec(preface);
        let header = u32::try_from(bytes.len()).unwrap().to_be_bytes();
        stream.write_all(&header[..2]).await.unwrap();
        stream.flush().await.unwrap();
        (stream, [header[2..].to_vec(), bytes].concat())
    }

    #[tokio::test]
    async fn both_sides_open_streams() {
        let (connector, acceptor) = carriers().await;
        let connector_open = {
            let connector = connector.clone();
            tokio::spawn(async move { connector.open_stream(preface(1)).await.unwrap() })
        };
        let (got, mut inbound) = acceptor.accept_stream().await.unwrap();
        assert_eq!(got, preface(1));
        let inbound_read = tokio::spawn(async move {
            let mut bytes = [0; 4];
            inbound.read_exact(&mut bytes).await.unwrap();
            bytes
        });
        let mut outbound = connector_open.await.unwrap();
        outbound.write_all(b"ping").await.unwrap();
        assert_eq!(inbound_read.await.unwrap(), *b"ping");

        let acceptor_open = {
            let acceptor = acceptor.clone();
            tokio::spawn(async move { acceptor.open_stream(preface(2)).await.unwrap() })
        };
        let (got, mut inbound) = connector.accept_stream().await.unwrap();
        assert_eq!(got, preface(2));
        let inbound_write = tokio::spawn(async move {
            inbound.write_all(b"pong").await.unwrap();
        });
        let mut outbound = acceptor_open.await.unwrap();
        let mut bytes = [0; 4];
        outbound.read_exact(&mut bytes).await.unwrap();
        inbound_write.await.unwrap();
        assert_eq!(bytes, *b"pong");
    }

    #[tokio::test]
    async fn refusal_code_survives_the_reset() {
        let (connector, acceptor) = carriers().await;
        let opening = tokio::spawn(async move { connector.open_stream(preface(3)).await });
        let (_, mut inbound) = acceptor.accept_stream().await.unwrap();
        inbound
            .reset(pb::StreamRefusal::PaymentRequired)
            .await
            .unwrap();
        assert!(matches!(
            opening.await.unwrap(),
            Err(OpenError::Refused(pb::StreamRefusal::PaymentRequired))
        ));
    }

    #[tokio::test]
    async fn finished_stream_reads_eof() {
        let (connector, acceptor) = carriers().await;
        let opening = tokio::spawn(async move { connector.open_stream(preface(4)).await.unwrap() });
        let (_, mut inbound) = acceptor.accept_stream().await.unwrap();
        inbound.finish().await.unwrap();
        let mut outbound = opening.await.unwrap();
        let mut byte = [0];
        assert_eq!(outbound.read(&mut byte).await.unwrap(), 0);
    }

    #[tokio::test]
    async fn inbound_stream_survives_a_cancelled_accept_before_its_preface_arrives() {
        let (connector, acceptor) = carriers().await;
        let expected = preface(5);
        let (mut outbound, rest) = open_with_partial_preface(&connector, &expected).await;

        assert!(
            tokio::time::timeout(Duration::from_millis(50), acceptor.accept_stream())
                .await
                .is_err()
        );

        outbound.write_all(&rest).await.unwrap();
        outbound.flush().await.unwrap();
        let (received, mut inbound) =
            tokio::time::timeout(Duration::from_secs(1), acceptor.accept_stream())
                .await
                .expect("cancelled accept must not consume the pending stream")
                .unwrap();
        assert_eq!(received, expected);
        inbound.flush().await.unwrap();
    }

    #[tokio::test]
    async fn stream_withholding_its_preface_does_not_block_later_accepts() {
        let (connector, acceptor) = carriers().await;
        let blocked_preface = preface(6);
        let (mut blocked, rest) = open_with_partial_preface(&connector, &blocked_preface).await;

        let expected = preface(7);
        let opening = {
            let connector = connector.clone();
            let expected = expected.clone();
            tokio::spawn(async move { connector.open_stream(expected).await.unwrap() })
        };
        let (received, mut inbound) =
            tokio::time::timeout(Duration::from_secs(1), acceptor.accept_stream())
                .await
                .expect("later stream must overtake a stream withholding its preface")
                .unwrap();
        assert_eq!(received, expected);
        inbound.flush().await.unwrap();
        opening.await.unwrap();

        blocked.write_all(&rest).await.unwrap();
        blocked.flush().await.unwrap();
        let (received, mut inbound) =
            tokio::time::timeout(Duration::from_secs(1), acceptor.accept_stream())
                .await
                .expect("withheld stream must be delivered once its preface arrives")
                .unwrap();
        assert_eq!(received, blocked_preface);
        inbound.flush().await.unwrap();
    }

    #[tokio::test]
    async fn ten_concurrent_streams_do_not_block_behind_a_slow_reader() {
        let (connector, acceptor) = carriers().await;
        let slow_open = {
            let connector = connector.clone();
            tokio::spawn(async move { connector.open_stream(preface(0)).await.unwrap() })
        };
        let (_, mut slow_inbound) = acceptor.accept_stream().await.unwrap();
        let slow_accept = tokio::spawn(async move {
            slow_inbound.flush().await.unwrap();
            tokio::time::sleep(Duration::from_millis(250)).await;
            let mut payload = Vec::new();
            slow_inbound.read_to_end(&mut payload).await.unwrap();
            payload
        });
        let mut slow_outbound = slow_open.await.unwrap();
        let slow_payload = vec![7; 512 * 1024];
        let slow_write = tokio::spawn(async move {
            slow_outbound.write_all(&slow_payload).await.unwrap();
            slow_outbound.finish().await.unwrap();
        });

        let opens = (1..=10)
            .map(|id| {
                let connector = connector.clone();
                tokio::spawn(async move { connector.open_stream(preface(id)).await.unwrap() })
            })
            .collect::<Vec<_>>();
        let mut inbound = Vec::new();
        for id in 1..=10 {
            let (got, mut stream) = acceptor.accept_stream().await.unwrap();
            assert_eq!(got, preface(id));
            inbound.push(tokio::spawn(async move {
                stream.flush().await.unwrap();
                let mut byte = [0];
                stream.read_exact(&mut byte).await.unwrap();
                byte[0]
            }));
        }
        for (id, opened) in opens.into_iter().enumerate() {
            let mut stream = opened.await.unwrap();
            stream.write_all(&[id as u8 + 1]).await.unwrap();
        }
        for (id, received) in inbound.into_iter().enumerate() {
            assert_eq!(received.await.unwrap(), id as u8 + 1);
        }

        slow_write.await.unwrap();
        assert_eq!(slow_accept.await.unwrap().len(), 512 * 1024);
    }

    #[tokio::test]
    async fn control_frames_round_trip_under_message_size_limit() {
        let (connector, acceptor) = new_carriers();
        let (mut connector_sink, mut connector_source) = connector.control();
        let (mut acceptor_sink, mut acceptor_source) = acceptor.control();
        let hello = pb::Message {
            body: Some(message::Body::Hello(pb::Hello {
                supported_protocol_versions: vec![2],
                host: None,
                neighbors: Vec::new(),
                auth_token: Some("token".into()),
            })),
        };
        let close = pb::Message {
            body: Some(message::Body::LinkClose(pb::LinkClose {
                reason: pb::LinkCloseReason::UserShutdown as i32,
                error: None,
            })),
        };

        write_message(&mut connector_sink, &hello).await.unwrap();
        assert_eq!(
            read_message(&mut acceptor_source).await.unwrap(),
            Some(hello)
        );
        write_message(&mut acceptor_sink, &close).await.unwrap();
        assert_eq!(
            read_message(&mut connector_source).await.unwrap(),
            Some(close)
        );
    }
}
