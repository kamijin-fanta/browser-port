use anyhow::{anyhow, Context};
use windows::core::PCWSTR;
use windows::Win32::Foundation::{CloseHandle, GetLastError, ERROR_ALREADY_EXISTS, HANDLE};
use windows::Win32::System::Threading::CreateMutexW;

pub struct SingleInstanceGuard {
    handle: HANDLE,
}

impl SingleInstanceGuard {
    pub fn acquire(name: &str) -> anyhow::Result<Option<Self>> {
        let wide_name = wide_null(name);
        unsafe {
            let handle =
                CreateMutexW(None, false, PCWSTR(wide_name.as_ptr())).context("CreateMutexW failed")?;
            if handle.is_invalid() {
                return Err(anyhow!("CreateMutexW returned invalid handle"));
            }

            if GetLastError() == ERROR_ALREADY_EXISTS {
                let _ = CloseHandle(handle);
                return Ok(None);
            }

            Ok(Some(Self { handle }))
        }
    }
}

impl Drop for SingleInstanceGuard {
    fn drop(&mut self) {
        unsafe {
            let _ = CloseHandle(self.handle);
        }
    }
}

fn wide_null(value: &str) -> Vec<u16> {
    value.encode_utf16().chain(std::iter::once(0)).collect()
}
