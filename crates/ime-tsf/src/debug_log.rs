//! Opt-in, privacy-safe TSF diagnostic trace logger (`%LOCALAPPDATA%\Pyn\tsf_debug.log`).
//!
//! Enabled only when the flag file `%LOCALAPPDATA%\Pyn\debug_tsf` exists.
//! Never perform any file I/O from `DllMain`, and never log user text or candidates.

use std::fmt::Arguments;
use std::fs::{self, OpenOptions};
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::OnceLock;
use std::time::{SystemTime, UNIX_EPOCH};

use windows::Win32::System::LibraryLoader::GetModuleFileNameW;
use windows::Win32::System::Threading::GetCurrentThreadId;

use crate::guids::active_guids;

const RECHECK_INTERVAL_MS: u64 = 2000;
const MAX_LOG_BYTES: u64 = 5 * 1024 * 1024;

static LOG_ENABLED: AtomicBool = AtomicBool::new(false);
static LAST_CHECK_MS: AtomicU64 = AtomicU64::new(0);
static PROC_NAME: OnceLock<String> = OnceLock::new();

fn pyn_data_dir() -> Option<PathBuf> {
    let base = std::env::var_os("LOCALAPPDATA")?;
    if base.is_empty() {
        return None;
    }
    Some(PathBuf::from(base).join("Pyn"))
}

fn flag_path() -> Option<PathBuf> {
    Some(pyn_data_dir()?.join("debug_tsf"))
}

fn log_path() -> Option<PathBuf> {
    Some(pyn_data_dir()?.join("tsf_debug.log"))
}

fn now_epoch_millis() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0)
}

/// Immediately re-checks whether `%LOCALAPPDATA%\Pyn\debug_tsf` exists and updates cached state.
pub fn refresh_enabled() -> bool {
    let enabled = flag_path().is_some_and(|p| p.exists());
    LOG_ENABLED.store(enabled, Ordering::Release);
    LAST_CHECK_MS.store(now_epoch_millis().max(1), Ordering::Relaxed);
    enabled
}

/// Returns whether TSF diagnostic logging is currently enabled (cached for 2000 ms).
pub fn is_enabled() -> bool {
    let now = now_epoch_millis();
    let last = LAST_CHECK_MS.load(Ordering::Relaxed);
    if last == 0 || now.saturating_sub(last) >= RECHECK_INTERVAL_MS {
        return refresh_enabled();
    }
    LOG_ENABLED.load(Ordering::Acquire)
}

fn host_process_name() -> &'static str {
    PROC_NAME
        .get_or_init(|| {
            let mut buf = [0u16; 512];
            let len = unsafe { GetModuleFileNameW(None, &mut buf) } as usize;
            if len == 0 || len >= buf.len() {
                return "unknown".to_owned();
            }
            let full = String::from_utf16_lossy(&buf[..len]);
            Path::new(&full)
                .file_name()
                .map(|s| s.to_string_lossy().into_owned())
                .unwrap_or(full)
        })
        .as_str()
}

pub fn format_entry(
    now_ms: u64,
    pid: u32,
    tid: u32,
    proc_name: &str,
    generation: u16,
    tag: &str,
    args: Arguments<'_>,
) -> String {
    let secs = now_ms / 1000;
    let ms = now_ms % 1000;
    format!(
        "[{secs}.{ms:03} pid={pid} tid={tid} proc={proc_name} gen=g{generation:04x}] {tag}: {args}\n"
    )
}

pub fn write_to_path(path: &Path, line: &str) {
    if let Ok(meta) = fs::metadata(path) {
        if meta.len() > MAX_LOG_BYTES {
            return;
        }
    }
    if let Some(parent) = path.parent() {
        let _ = fs::create_dir_all(parent);
    }
    if let Ok(mut file) = OpenOptions::new().create(true).append(true).open(path) {
        let _ = file.write_all(line.as_bytes());
    }
}

pub fn log_line(tag: &str, args: Arguments<'_>) {
    if !is_enabled() {
        return;
    }
    let Some(path) = log_path() else {
        return;
    };
    let line = format_entry(
        now_epoch_millis(),
        std::process::id(),
        unsafe { GetCurrentThreadId() },
        host_process_name(),
        active_guids().generation,
        tag,
        args,
    );
    write_to_path(&path, &line);
}

macro_rules! tsf_log {
    ($tag:expr, $($arg:tt)+) => {{
        if $crate::debug_log::is_enabled() {
            $crate::debug_log::log_line($tag, ::core::format_args!($($arg)+));
        }
    }};
}

pub(crate) use tsf_log;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn format_entry_and_write_cap() {
        let line = format_entry(
            1_700_000_000_042,
            123,
            456,
            "chrome.exe",
            0x000b,
            "focus",
            format_args!("top_ctx={} hr=0x{:08x}", true, 0u32),
        );
        assert_eq!(
            line,
            "[1700000000.042 pid=123 tid=456 proc=chrome.exe gen=g000b] focus: top_ctx=true hr=0x00000000\n"
        );

        let path = std::env::temp_dir().join(format!(
            "pyn_tsf_debug_log_test_{}_{}.log",
            std::process::id(),
            now_epoch_millis()
        ));
        let _ = fs::remove_file(&path);

        write_to_path(&path, &line);
        write_to_path(&path, &line);
        let content = fs::read_to_string(&path).expect("read test log");
        assert_eq!(content.lines().count(), 2);

        // Pre-seed past MAX_LOG_BYTES and confirm further writes are skipped.
        {
            let f = OpenOptions::new().write(true).open(&path).unwrap();
            f.set_len(MAX_LOG_BYTES + 1).unwrap();
        }
        write_to_path(&path, &line);
        assert_eq!(fs::metadata(&path).unwrap().len(), MAX_LOG_BYTES + 1);

        let _ = fs::remove_file(&path);
    }
}
