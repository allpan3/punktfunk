//! Typed local control protocol over one TLS stream per request.
//!
//! Frames are a four-byte big-endian length followed by JSON and are capped at
//! 64 KiB before allocation. Requests and responses carry an explicit schema
//! version. Commands and results are tagged enums; errors have stable codes,
//! a message, and a retry hint. The bearer is redacted from `Debug` output.
//! Transport and TLS live in `control`; this module is usable with in-memory
//! readers and writers for framing and dispatch tests.

use crate::model::{CreateSeat, Seat, SeatId};
use serde::de::DeserializeOwned;
use serde::{Deserialize, Serialize};
use std::fmt;
use std::io::{Read, Write};

pub const CONTROL_SCHEMA_VERSION: u32 = 1;
pub const MAX_FRAME_BYTES: usize = 64 * 1024;

#[derive(Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(transparent)]
pub struct BearerToken(String);

impl BearerToken {
    pub fn new(value: impl Into<String>) -> Result<Self, TokenError> {
        let value = value.into();
        if value.len() < 16
            || value.len() > 256
            || !value.bytes().all(|byte| byte.is_ascii_graphic())
        {
            return Err(TokenError);
        }
        Ok(Self(value))
    }

    pub fn expose(&self) -> &str {
        &self.0
    }
}

impl fmt::Debug for BearerToken {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("BearerToken(<redacted>)")
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, thiserror::Error)]
#[error("bearer token must be 16..=256 visible ASCII bytes")]
pub struct TokenError;

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum Command {
    List,
    Create {
        name: String,
        account: String,
        #[serde(default)]
        autostart: bool,
    },
    Start {
        id: SeatId,
    },
    Stop {
        id: SeatId,
    },
    Delete {
        id: SeatId,
    },
    Doctor,
}

impl Command {
    pub fn create(request: CreateSeat) -> Self {
        Self::Create {
            name: request.name,
            account: request.account,
            autostart: request.autostart,
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Request {
    pub schema_version: u32,
    pub bearer: BearerToken,
    pub command: Command,
}

impl Request {
    pub fn new(bearer: BearerToken, command: Command) -> Self {
        Self {
            schema_version: CONTROL_SCHEMA_VERSION,
            bearer,
            command,
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum CommandResult {
    List { seats: Vec<Seat> },
    Created { seat: Seat },
    Started { seat: Seat },
    Stopped { seat: Seat },
    Deleted { id: SeatId },
    Doctor { report: DoctorReport },
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "status", rename_all = "snake_case")]
pub enum Response {
    Success {
        schema_version: u32,
        result: CommandResult,
    },
    Error {
        schema_version: u32,
        error: ApiError,
    },
}

impl Response {
    pub fn success(result: CommandResult) -> Self {
        Self::Success {
            schema_version: CONTROL_SCHEMA_VERSION,
            result,
        }
    }

    pub fn error(error: ApiError) -> Self {
        Self::Error {
            schema_version: CONTROL_SCHEMA_VERSION,
            error,
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ApiError {
    pub code: ErrorCode,
    pub message: String,
    #[serde(default)]
    pub retryable: bool,
}

impl ApiError {
    pub fn new(code: ErrorCode, message: impl Into<String>) -> Self {
        Self {
            code,
            message: message.into(),
            retryable: false,
        }
    }

    pub fn retryable(mut self) -> Self {
        self.retryable = true;
        self
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ErrorCode {
    Unauthorized,
    SchemaVersion,
    InvalidRequest,
    Capacity,
    Conflict,
    NotFound,
    Backend,
    Persistence,
    FrameTooLarge,
    MalformedFrame,
    Transport,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct DoctorReport {
    pub healthy: bool,
    pub diagnostics: Vec<Diagnostic>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Diagnostic {
    pub level: DiagnosticLevel,
    pub code: String,
    pub message: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub seat_id: Option<SeatId>,
}

impl Diagnostic {
    pub fn info(code: impl Into<String>, message: impl Into<String>) -> Self {
        Self {
            level: DiagnosticLevel::Info,
            code: code.into(),
            message: message.into(),
            seat_id: None,
        }
    }

    pub fn error(code: impl Into<String>, message: impl Into<String>) -> Self {
        Self {
            level: DiagnosticLevel::Error,
            code: code.into(),
            message: message.into(),
            seat_id: None,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DiagnosticLevel {
    Info,
    Warning,
    Error,
}

pub fn read_json_frame<R, T>(reader: &mut R) -> Result<T, FrameError>
where
    R: Read,
    T: DeserializeOwned,
{
    let bytes = read_frame_bytes(reader)?;
    serde_json::from_slice(&bytes).map_err(FrameError::Decode)
}

pub fn write_json_frame<W, T>(writer: &mut W, value: &T) -> Result<(), FrameError>
where
    W: Write,
    T: Serialize,
{
    let bytes = serde_json::to_vec(value).map_err(FrameError::Encode)?;
    write_frame_bytes(writer, &bytes)
}

pub fn read_frame_bytes<R: Read>(reader: &mut R) -> Result<Vec<u8>, FrameError> {
    let mut prefix = [0_u8; 4];
    reader.read_exact(&mut prefix)?;
    let length = u32::from_be_bytes(prefix) as usize;
    if length > MAX_FRAME_BYTES {
        return Err(FrameError::TooLarge(length));
    }
    let mut payload = vec![0_u8; length];
    reader.read_exact(&mut payload)?;
    Ok(payload)
}

pub fn write_frame_bytes<W: Write>(writer: &mut W, payload: &[u8]) -> Result<(), FrameError> {
    if payload.len() > MAX_FRAME_BYTES {
        return Err(FrameError::TooLarge(payload.len()));
    }
    writer.write_all(&(payload.len() as u32).to_be_bytes())?;
    writer.write_all(payload)?;
    writer.flush()?;
    Ok(())
}

#[derive(Debug, thiserror::Error)]
pub enum FrameError {
    #[error("control frame I/O failed: {0}")]
    Io(#[from] std::io::Error),
    #[error("control frame is {0} bytes; the limit is 65536")]
    TooLarge(usize),
    #[error("control JSON encoding failed: {0}")]
    Encode(serde_json::Error),
    #[error("control JSON decoding failed: {0}")]
    Decode(serde_json::Error),
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Cursor;

    #[test]
    fn framing_accepts_the_cap_and_rejects_one_byte_more() {
        let exact = vec![7_u8; MAX_FRAME_BYTES];
        let mut framed = Vec::new();
        write_frame_bytes(&mut framed, &exact).unwrap();
        assert_eq!(read_frame_bytes(&mut Cursor::new(framed)).unwrap(), exact);

        let oversized = vec![0_u8; MAX_FRAME_BYTES + 1];
        assert!(matches!(
            write_frame_bytes(&mut Vec::new(), &oversized),
            Err(FrameError::TooLarge(size)) if size == MAX_FRAME_BYTES + 1
        ));
        let prefix_only = ((MAX_FRAME_BYTES + 1) as u32).to_be_bytes();
        assert!(matches!(
            read_frame_bytes(&mut Cursor::new(prefix_only)),
            Err(FrameError::TooLarge(size)) if size == MAX_FRAME_BYTES + 1
        ));
    }

    #[test]
    fn request_round_trips_without_debugging_the_bearer() {
        let token = BearerToken::new("0123456789abcdef").unwrap();
        let request = Request::new(token, Command::List);
        let mut frame = Vec::new();
        write_json_frame(&mut frame, &request).unwrap();
        let decoded: Request = read_json_frame(&mut Cursor::new(frame)).unwrap();
        assert_eq!(decoded, request);
        assert!(!format!("{request:?}").contains("0123456789abcdef"));
    }
}
