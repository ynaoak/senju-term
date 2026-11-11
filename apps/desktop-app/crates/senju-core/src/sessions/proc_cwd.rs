//! Reads another process's current directory — the cwd source for cmd.exe.
//!
//! cmd.exe has no prompt hook to write a cwd file from, and the inbox ConPTY
//! drops the OSC 7 a hook would emit anyway. The directory is still known to
//! the OS, though: it lives in the child's PEB, in
//! `RTL_USER_PROCESS_PARAMETERS.CurrentDirectory`. There is no documented API
//! for reading it, so this walks the (stable, widely relied-upon) layout with
//! `ReadProcessMemory`. Offsets below are the 64-bit ones; 32-bit builds
//! return `None` rather than guess at a second layout.

/// The shell's current directory, or `None` if it cannot be read (process
/// gone, access denied, unsupported target).
pub fn process_cwd(pid: u32) -> Option<String> {
    imp::process_cwd(pid)
}

/// Drops the trailing backslash the OS keeps on the directory, except for a
/// drive root where `C:` would mean "the cwd of drive C" rather than its root.
#[cfg_attr(not(all(windows, target_pointer_width = "64")), allow(dead_code))]
fn trim_dos_path(s: &str) -> String {
    let bytes = s.as_bytes();
    let is_drive_root = bytes.len() == 3 && bytes[1] == b':' && bytes[2] == b'\\';
    if s.len() > 1 && s.ends_with('\\') && !is_drive_root {
        s[..s.len() - 1].to_string()
    } else {
        s.to_string()
    }
}

#[cfg(all(windows, target_pointer_width = "64"))]
mod imp {
    use super::trim_dos_path;
    use std::ffi::c_void;
    use windows_sys::Wdk::System::Threading::{NtQueryInformationProcess, ProcessBasicInformation};
    use windows_sys::Win32::Foundation::{CloseHandle, HANDLE};
    use windows_sys::Win32::System::Diagnostics::Debug::ReadProcessMemory;
    use windows_sys::Win32::System::Threading::{
        OpenProcess, PROCESS_QUERY_INFORMATION, PROCESS_VM_READ,
    };

    /// Own definition: windows-sys only exposes the full struct behind an
    /// extra feature, and only `PebBaseAddress` is used.
    #[repr(C)]
    struct ProcessBasicInformation64 {
        exit_status: i32,
        peb_base_address: usize,
        affinity_mask: usize,
        base_priority: i32,
        unique_process_id: usize,
        inherited_from: usize,
    }

    const PEB_PROCESS_PARAMETERS: usize = 0x20;
    const PARAMS_CURRENT_DIRECTORY: usize = 0x38;

    struct Handle(HANDLE);
    impl Drop for Handle {
        fn drop(&mut self) {
            // SAFETY: the handle came from a successful OpenProcess.
            unsafe { CloseHandle(self.0) };
        }
    }

    fn read<T: Copy + Default>(h: HANDLE, addr: usize) -> Option<T> {
        let mut out = T::default();
        let mut got = 0usize;
        // SAFETY: `out` is a valid buffer of size_of::<T>() bytes; a bad
        // remote address just makes the call fail.
        let ok = unsafe {
            ReadProcessMemory(
                h,
                addr as *const c_void,
                &mut out as *mut T as *mut c_void,
                std::mem::size_of::<T>(),
                &mut got,
            )
        };
        (ok != 0 && got == std::mem::size_of::<T>()).then_some(out)
    }

    pub fn process_cwd(pid: u32) -> Option<String> {
        // SAFETY: plain FFI; a null handle means failure.
        let raw = unsafe { OpenProcess(PROCESS_QUERY_INFORMATION | PROCESS_VM_READ, 0, pid) };
        if raw.is_null() {
            return None;
        }
        let proc = Handle(raw);
        let h = proc.0;

        let mut pbi = ProcessBasicInformation64 {
            exit_status: 0,
            peb_base_address: 0,
            affinity_mask: 0,
            base_priority: 0,
            unique_process_id: 0,
            inherited_from: 0,
        };
        let mut ret = 0u32;
        // SAFETY: `pbi` matches the 64-bit PROCESS_BASIC_INFORMATION layout
        // and the length passed is its size.
        let status = unsafe {
            NtQueryInformationProcess(
                h,
                ProcessBasicInformation,
                &mut pbi as *mut _ as *mut c_void,
                std::mem::size_of::<ProcessBasicInformation64>() as u32,
                &mut ret,
            )
        };
        if status < 0 || pbi.peb_base_address == 0 {
            return None;
        }
        let params: usize = read(h, pbi.peb_base_address + PEB_PROCESS_PARAMETERS)?;
        if params == 0 {
            return None;
        }
        // UNICODE_STRING: u16 Length, u16 MaximumLength, 4 bytes padding, ptr.
        let len: u16 = read(h, params + PARAMS_CURRENT_DIRECTORY)?;
        let buf: usize = read(h, params + PARAMS_CURRENT_DIRECTORY + 8)?;
        if len == 0 || buf == 0 || len & 1 != 0 {
            return None;
        }
        let mut units = vec![0u16; len as usize / 2];
        let mut got = 0usize;
        // SAFETY: `units` holds exactly `len` bytes.
        let ok = unsafe {
            ReadProcessMemory(
                h,
                buf as *const c_void,
                units.as_mut_ptr() as *mut c_void,
                len as usize,
                &mut got,
            )
        };
        if ok == 0 || got != len as usize {
            return None;
        }
        let s = String::from_utf16(&units).ok()?;
        let s = trim_dos_path(&s);
        (!s.is_empty()).then_some(s)
    }
}

#[cfg(not(all(windows, target_pointer_width = "64")))]
mod imp {
    pub fn process_cwd(_pid: u32) -> Option<String> {
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn trailing_backslash_is_trimmed_except_on_drive_roots() {
        assert_eq!(trim_dos_path(r"C:\Users\me\"), r"C:\Users\me");
        assert_eq!(trim_dos_path(r"C:\Users\me"), r"C:\Users\me");
        assert_eq!(trim_dos_path(r"C:\"), r"C:\");
        assert_eq!(trim_dos_path(r"\\server\share\"), r"\\server\share");
    }

    #[cfg(not(windows))]
    #[test]
    fn unsupported_targets_return_none() {
        assert_eq!(process_cwd(1), None);
    }
}
