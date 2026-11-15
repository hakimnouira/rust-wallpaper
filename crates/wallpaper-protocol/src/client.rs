//! Reusable asynchronous Windows named-pipe client.
//!
//! A UI or CLI process can own this client; it never owns daemon runtime state.

use std::io;
use std::time::Duration;
use thiserror::Error;
use tokio::io::{AsyncBufRead, AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::net::windows::named_pipe::{ClientOptions, NamedPipeClient};
use tokio::time::sleep;

use crate::{RequestEnvelope, ResponseEnvelope, MAX_FRAME_BYTES, PROTOCOL_VERSION};

pub const PIPE_NAME: &str = r"\\.\pipe\rust-wallpaper-ipc";

#[derive(Debug, Error)]
pub enum ClientError {
    #[error("named-pipe connection failed: {0}")]
    Connect(#[source] io::Error),
    #[error("named-pipe I/O failed: {0}")]
    Io(#[from] io::Error),
    #[error("invalid response envelope: {0}")]
    Json(#[from] serde_json::Error),
    #[error("daemon response protocol version {received} is not supported (expected {supported})")]
    VersionMismatch { received: u16, supported: u16 },
    #[error("response request ID did not match the request")]
    RequestIdMismatch,
    #[error("daemon closed the pipe before replying")]
    Disconnected,
    #[error("response frame exceeded {MAX_FRAME_BYTES} bytes")]
    FrameTooLarge,
}

pub struct IpcClient {
    reader: BufReader<tokio::io::ReadHalf<NamedPipeClient>>,
    writer: tokio::io::WriteHalf<NamedPipeClient>,
}

impl IpcClient {
    pub async fn connect() -> Result<Self, ClientError> {
        let stream = ClientOptions::new()
            .open(PIPE_NAME)
            .map_err(ClientError::Connect)?;
        let (reader, writer) = tokio::io::split(stream);
        Ok(Self {
            reader: BufReader::new(reader),
            writer,
        })
    }

    /// Attempts to connect with bounded exponential backoff. The first attempt is immediate.
    pub async fn connect_with_retry(
        attempts: usize,
        initial_backoff: Duration,
    ) -> Result<Self, ClientError> {
        let attempts = attempts.max(1);
        let mut delay = initial_backoff;
        let mut last_error = None;
        for attempt in 0..attempts {
            match Self::connect().await {
                Ok(client) => return Ok(client),
                Err(error) => last_error = Some(error),
            }
            if attempt + 1 < attempts {
                sleep(delay).await;
                delay = (delay * 2).min(Duration::from_secs(5));
            }
        }
        Err(last_error.expect("at least one connection attempt was made"))
    }

    pub async fn request(
        &mut self,
        request: &RequestEnvelope,
    ) -> Result<ResponseEnvelope, ClientError> {
        let encoded = serde_json::to_vec(request)?;
        if encoded.len() > MAX_FRAME_BYTES {
            return Err(ClientError::FrameTooLarge);
        }
        self.writer.write_all(&encoded).await?;
        self.writer.write_all(b"\n").await?;
        self.writer.flush().await?;

        let frame = read_bounded_frame(&mut self.reader)
            .await?
            .ok_or(ClientError::Disconnected)?;
        let response: ResponseEnvelope = serde_json::from_slice(&frame)?;
        if response.protocol_version != PROTOCOL_VERSION {
            return Err(ClientError::VersionMismatch {
                received: response.protocol_version,
                supported: PROTOCOL_VERSION,
            });
        }
        if response.request_id != request.request_id {
            return Err(ClientError::RequestIdMismatch);
        }
        Ok(response)
    }
}

async fn read_bounded_frame<R: AsyncBufRead + Unpin>(
    reader: &mut R,
) -> Result<Option<Vec<u8>>, ClientError> {
    let mut frame = Vec::with_capacity(256);
    loop {
        let available = reader.fill_buf().await?;
        if available.is_empty() {
            return if frame.is_empty() {
                Ok(None)
            } else {
                Err(ClientError::Disconnected)
            };
        }
        if let Some(newline) = available.iter().position(|byte| *byte == b'\n') {
            if frame.len() + newline > MAX_FRAME_BYTES {
                return Err(ClientError::FrameTooLarge);
            }
            frame.extend_from_slice(&available[..newline]);
            reader.consume(newline + 1);
            return Ok(Some(frame));
        }
        if frame.len() + available.len() > MAX_FRAME_BYTES {
            return Err(ClientError::FrameTooLarge);
        }
        let consumed = available.len();
        frame.extend_from_slice(available);
        reader.consume(consumed);
    }
}
