//! Win32 Named Pipe server/client with AppContainer-accessible DACL and detached launcher.

use std::io::{self, Read, Write};
use std::os::windows::process::CommandExt;
use std::path::Path;
use std::process::{Command, Stdio};

use windows::Win32::Foundation::{
    CloseHandle, LocalFree, ERROR_BROKEN_PIPE, ERROR_PIPE_BUSY, ERROR_PIPE_CONNECTED,
    ERROR_PIPE_NOT_CONNECTED, GENERIC_READ, GENERIC_WRITE, HANDLE, HLOCAL, INVALID_HANDLE_VALUE,
};
use windows::Win32::Security::Authorization::ConvertStringSecurityDescriptorToSecurityDescriptorW;
use windows::Win32::Security::{PSECURITY_DESCRIPTOR, SECURITY_ATTRIBUTES};
use windows::Win32::Storage::FileSystem::{
    CreateFileW, FlushFileBuffers, ReadFile, WriteFile, FILE_ATTRIBUTE_NORMAL,
    FILE_FLAGS_AND_ATTRIBUTES, FILE_SHARE_NONE, OPEN_EXISTING, PIPE_ACCESS_DUPLEX,
};
use windows::Win32::System::Pipes::{
    ConnectNamedPipe, CreateNamedPipeW, DisconnectNamedPipe, WaitNamedPipeW, PIPE_READMODE_BYTE,
    PIPE_TYPE_BYTE, PIPE_UNLIMITED_INSTANCES, PIPE_WAIT,
};
use windows_core::PCWSTR;

use crate::protocol::{read_frame, write_frame, IpcRequest, IpcResponse};

const SDDL_REVISION_1: u32 = 1;
const PIPE_SDDL: &str = "D:(A;;GA;;;BA)(A;;GA;;;SY)(A;;GA;;;AU)(A;;GRGW;;;AC)S:(ML;;NW;;;LW)";
const CREATE_NO_WINDOW: u32 = 0x0800_0000;

pub fn to_wide_null(s: &str) -> Vec<u16> {
    s.encode_utf16().chain(std::iter::once(0)).collect()
}

pub struct NamedPipeServerInstance {
    handle: HANDLE,
}

unsafe impl Send for NamedPipeServerInstance {}

impl NamedPipeServerInstance {
    pub fn create(pipe_name: &str) -> io::Result<Self> {
        let wide_name = to_wide_null(pipe_name);
        let wide_sddl = to_wide_null(PIPE_SDDL);
        let mut sd = PSECURITY_DESCRIPTOR::default();

        unsafe {
            ConvertStringSecurityDescriptorToSecurityDescriptorW(
                PCWSTR(wide_sddl.as_ptr()),
                SDDL_REVISION_1,
                &mut sd,
                None,
            )
            .map_err(|e| io::Error::from_raw_os_error(e.code().0))?;
        }

        let sa = SECURITY_ATTRIBUTES {
            nLength: std::mem::size_of::<SECURITY_ATTRIBUTES>() as u32,
            lpSecurityDescriptor: sd.0,
            bInheritHandle: false.into(),
        };

        let handle = unsafe {
            CreateNamedPipeW(
                PCWSTR(wide_name.as_ptr()),
                FILE_FLAGS_AND_ATTRIBUTES(PIPE_ACCESS_DUPLEX.0),
                PIPE_TYPE_BYTE | PIPE_READMODE_BYTE | PIPE_WAIT,
                PIPE_UNLIMITED_INSTANCES,
                65536,
                65536,
                0,
                Some(&sa),
            )
        };

        unsafe {
            let _ = LocalFree(Some(HLOCAL(sd.0)));
        }

        if handle == INVALID_HANDLE_VALUE || handle.is_invalid() {
            return Err(io::Error::last_os_error());
        }
        Ok(Self { handle })
    }

    pub fn accept(&self) -> io::Result<()> {
        match unsafe { ConnectNamedPipe(self.handle, None) } {
            Ok(()) => Ok(()),
            Err(e) if e.code() == ERROR_PIPE_CONNECTED.to_hresult() => Ok(()),
            Err(e) => Err(io::Error::from_raw_os_error(e.code().0)),
        }
    }
}

impl Read for NamedPipeServerInstance {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        pipe_read(self.handle, buf)
    }
}

impl Write for NamedPipeServerInstance {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        pipe_write(self.handle, buf)
    }

    fn flush(&mut self) -> io::Result<()> {
        unsafe { FlushFileBuffers(self.handle).map_err(|e| io::Error::from_raw_os_error(e.code().0)) }
    }
}

impl Drop for NamedPipeServerInstance {
    fn drop(&mut self) {
        unsafe {
            let _ = FlushFileBuffers(self.handle);
            let _ = DisconnectNamedPipe(self.handle);
            let _ = CloseHandle(self.handle);
        }
    }
}

pub struct NamedPipeClient {
    handle: HANDLE,
}

unsafe impl Send for NamedPipeClient {}

impl NamedPipeClient {
    pub fn connect(pipe_name: &str, wait_timeout_ms: u32) -> io::Result<Self> {
        let wide_name = to_wide_null(pipe_name);
        match open_pipe_handle(&wide_name) {
            Ok(handle) => Ok(Self { handle }),
            Err(e) if e.code() == ERROR_PIPE_BUSY.to_hresult() => {
                let waited = unsafe { WaitNamedPipeW(PCWSTR(wide_name.as_ptr()), wait_timeout_ms) };
                if !waited.as_bool() {
                    return Err(io::Error::last_os_error());
                }
                open_pipe_handle(&wide_name)
                    .map(|handle| Self { handle })
                    .map_err(|err| io::Error::from_raw_os_error(err.code().0))
            }
            Err(e) => Err(io::Error::from_raw_os_error(e.code().0)),
        }
    }

    pub fn request(&mut self, req: &IpcRequest) -> io::Result<IpcResponse> {
        write_frame(self, req)?;
        read_frame(self)
    }
}

impl Read for NamedPipeClient {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        pipe_read(self.handle, buf)
    }
}

impl Write for NamedPipeClient {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        pipe_write(self.handle, buf)
    }

    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

impl Drop for NamedPipeClient {
    fn drop(&mut self) {
        unsafe {
            let _ = CloseHandle(self.handle);
        }
    }
}

pub fn spawn_server_detached(exe_path: &Path) -> io::Result<()> {
    Command::new(exe_path)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .creation_flags(CREATE_NO_WINDOW)
        .spawn()?;
    Ok(())
}

fn open_pipe_handle(wide_name: &[u16]) -> windows_core::Result<HANDLE> {
    unsafe {
        CreateFileW(
            PCWSTR(wide_name.as_ptr()),
            GENERIC_READ.0 | GENERIC_WRITE.0,
            FILE_SHARE_NONE,
            None,
            OPEN_EXISTING,
            FILE_ATTRIBUTE_NORMAL,
            None,
        )
    }
}

fn pipe_read(handle: HANDLE, buf: &mut [u8]) -> io::Result<usize> {
    if buf.is_empty() {
        return Ok(0);
    }
    let mut read = 0u32;
    match unsafe { ReadFile(handle, Some(buf), Some(&mut read), None) } {
        Ok(()) => Ok(read as usize),
        Err(e)
            if e.code() == ERROR_BROKEN_PIPE.to_hresult()
                || e.code() == ERROR_PIPE_NOT_CONNECTED.to_hresult() =>
        {
            Ok(0)
        }
        Err(e) => Err(io::Error::from_raw_os_error(e.code().0)),
    }
}

fn pipe_write(handle: HANDLE, buf: &[u8]) -> io::Result<usize> {
    if buf.is_empty() {
        return Ok(0);
    }
    let mut written = 0u32;
    unsafe { WriteFile(handle, Some(buf), Some(&mut written), None) }
        .map(|()| written as usize)
        .map_err(|e| io::Error::from_raw_os_error(e.code().0))
}
