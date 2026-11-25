//! Length-prefixed JSON wire protocol between `ime_tsf.dll` and `ime-server.exe`.

use std::io::{self, Read, Write};

use serde::de::DeserializeOwned;
use serde::{Deserialize, Serialize};

pub const DEFAULT_PIPE_NAME: &str = r"\\.\pipe\PynTSF_v1";
pub const LEGACY_PIPE_NAME: &str = r"\\.\pipe\InputEngineTSF_v1";
pub const MAX_FRAME_BYTES: usize = 1024 * 1024;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "op", rename_all = "snake_case")]
pub enum IpcRequest {
    Ping,
    Convert {
        session_id: u64,
        context: String,
        keys: String,
        limit: usize,
    },
    Commit {
        session_id: u64,
        context: String,
        keys: String,
        text: String,
    },
    Undo {
        session_id: u64,
    },
    ResetSession {
        session_id: u64,
    },
    GetConfig,
    SetConfig {
        config: crate::config::ImeConfig,
    },
    Stats,
    Shutdown,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum IpcResponse {
    Pong {
        version: String,
        model_loaded: bool,
        reranker_loaded: bool,
        learning_enabled: bool,
    },
    Candidates {
        candidates: Vec<String>,
        latency_ms: f32,
        #[serde(default)]
        config_revision: u64,
    },
    Config {
        config: crate::config::ImeConfig,
        revision: u64,
    },
    Ack {
        ok: bool,
    },
    Stats {
        total_steps: u64,
        active_vocab_words: usize,
        active_oov_words: usize,
        staged_words: usize,
        active_bigrams: usize,
        uptime_secs: u64,
    },
    Error {
        message: String,
    },
}

pub fn write_frame<W: Write, T: Serialize>(w: &mut W, msg: &T) -> io::Result<()> {
    let payload = serde_json::to_vec(msg)
        .map_err(|e| io::Error::new(io::ErrorKind::InvalidData, format!("json encode: {e}")))?;
    if payload.len() > MAX_FRAME_BYTES {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "ipc frame exceeds MAX_FRAME_BYTES",
        ));
    }
    let len_bytes = (payload.len() as u32).to_le_bytes();
    w.write_all(&len_bytes)?;
    w.write_all(&payload)?;
    w.flush()
}

pub fn read_frame<R: Read, T: DeserializeOwned>(r: &mut R) -> io::Result<T> {
    let mut len_buf = [0u8; 4];
    r.read_exact(&mut len_buf)?;
    let len = u32::from_le_bytes(len_buf) as usize;
    if len == 0 || len > MAX_FRAME_BYTES {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            format!("invalid ipc frame length: {len}"),
        ));
    }
    let mut payload = vec![0u8; len];
    r.read_exact(&mut payload)?;
    serde_json::from_slice(&payload)
        .map_err(|e| io::Error::new(io::ErrorKind::InvalidData, format!("json decode: {e}")))
}
