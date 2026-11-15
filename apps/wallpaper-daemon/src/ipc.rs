//! Windows named-pipe transport for daemon IPC.
//!
//! The transport thread never touches runtime state. It forwards typed requests to
//! the daemon's main loop and waits for that loop to produce the response.

use std::sync::mpsc::{self, Receiver, SyncSender};
use tokio::io::{AsyncBufRead, AsyncBufReadExt, AsyncWrite, AsyncWriteExt, BufReader};
use tokio::net::windows::named_pipe::ServerOptions;
use tokio::sync::oneshot;
use tokio::time::{timeout, Duration};
use wallpaper_protocol::{
    ErrorCode, ProtocolError, RequestEnvelope, ResponseEnvelope, MAX_FRAME_BYTES,
};

pub const PIPE_NAME: &str = r"\\.\pipe\rust-wallpaper-ipc";
const REQUEST_QUEUE_CAPACITY: usize = 64;
const RESPONSE_TIMEOUT: Duration = Duration::from_secs(10);

pub struct PendingRequest {
    pub request: RequestEnvelope,
    pub reply: oneshot::Sender<ResponseEnvelope>,
}

/// Starts the named-pipe listener and returns the bounded queue consumed by the
/// daemon's main thread. The listener reconnects after transient pipe errors.
pub fn start() -> Receiver<PendingRequest> {
    let (tx, rx) = mpsc::sync_channel(REQUEST_QUEUE_CAPACITY);
    std::thread::Builder::new()
        .name("wallpaper-ipc-server".into())
        .spawn(move || {
            let runtime = match tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()
            {
                Ok(runtime) => runtime,
                Err(error) => {
                    tracing::error!(%error, "Could not create IPC runtime");
                    return;
                }
            };
            runtime.block_on(listener_loop(tx));
        })
        .expect("failed to start IPC server thread");
    rx
}

async fn listener_loop(tx: SyncSender<PendingRequest>) {
    let mut first_instance = true;
    loop {
        let mut options = ServerOptions::new();
        options.first_pipe_instance(first_instance);
        options.reject_remote_clients(true);
        let mut server = match options.create(PIPE_NAME) {
            Ok(server) => {
                first_instance = false;
                server
            }
            Err(error) => {
                tracing::warn!(%error, "Could not create IPC named pipe; retrying");
                tokio::time::sleep(Duration::from_secs(1)).await;
                continue;
            }
        };

        if let Err(error) = server.connect().await {
            tracing::warn!(%error, "IPC client connection failed");
            continue;
        }

        let client_tx = tx.clone();
        tokio::spawn(async move {
            if let Err(error) = serve_client(server, client_tx).await {
                tracing::debug!(%error, "IPC client disconnected");
            }
        });
    }
}

async fn read_bounded_frame<R: AsyncBufRead + Unpin>(
    reader: &mut R,
) -> Result<Option<Vec<u8>>, ProtocolError> {
    let mut frame = Vec::with_capacity(256);
    loop {
        let available = reader.fill_buf().await?;
        if available.is_empty() {
            return if frame.is_empty() {
                Ok(None)
            } else {
                Err(ProtocolError::UnexpectedEof)
            };
        }

        if let Some(newline) = available.iter().position(|byte| *byte == b'\n') {
            if frame.len() + newline > MAX_FRAME_BYTES {
                return Err(ProtocolError::FrameTooLarge);
            }
            frame.extend_from_slice(&available[..newline]);
            reader.consume(newline + 1);
            return Ok(Some(frame));
        }

        if frame.len() + available.len() > MAX_FRAME_BYTES {
            return Err(ProtocolError::FrameTooLarge);
        }
        let consumed = available.len();
        frame.extend_from_slice(available);
        reader.consume(consumed);
    }
}
async fn write_envelope<W: AsyncWrite + Unpin>(
    writer: &mut W,
    response: &ResponseEnvelope,
) -> Result<(), ProtocolError> {
    let bytes = serde_json::to_vec(response)?;
    if bytes.len() > MAX_FRAME_BYTES {
        return Err(ProtocolError::FrameTooLarge);
    }
    writer.write_all(&bytes).await?;
    writer.write_all(b"\n").await?;
    writer.flush().await?;
    Ok(())
}

async fn serve_client(
    stream: tokio::net::windows::named_pipe::NamedPipeServer,
    tx: SyncSender<PendingRequest>,
) -> Result<(), ProtocolError> {
    let (reader, mut writer) = tokio::io::split(stream);
    let mut reader = BufReader::new(reader);
    while let Some(frame) = read_bounded_frame(&mut reader).await? {
        let request: RequestEnvelope = match serde_json::from_slice(&frame) {
            Ok(request) => request,
            Err(error) => {
                tracing::debug!(%error, "Discarding malformed IPC request");
                break;
            }
        };

        if let Err(ProtocolError::VersionMismatch { received, supported }) =
            request.validate()
        {
            let response = ResponseEnvelope::error(
                request.request_id,
                ErrorCode::ProtocolMismatch,
                format!("client protocol {received}; daemon supports {supported}"),
            );
            write_envelope(&mut writer, &response).await?;
            continue;
        }

        let request_id = request.request_id;
        let (reply_tx, reply_rx) = oneshot::channel();
        let pending = PendingRequest {
            request,
            reply: reply_tx,
        };
        if tx.try_send(pending).is_err() {
            let response = ResponseEnvelope::error(
                request_id,
                ErrorCode::RuntimeFailure,
                "daemon request queue is busy",
            );
            write_envelope(&mut writer, &response).await?;
            continue;
        }

        let response = match timeout(RESPONSE_TIMEOUT, reply_rx).await {
            Ok(Ok(response)) => response,
            Ok(Err(_)) => ResponseEnvelope::error(
                request_id,
                ErrorCode::Internal,
                "daemon closed the request without a response",
            ),
            Err(_) => ResponseEnvelope::error(
                request_id,
                ErrorCode::RuntimeFailure,
                "daemon request timed out",
            ),
        };
        write_envelope(&mut writer, &response).await?;
    }
    Ok(())
}
