//! Starting a process the CLI will not wait for: the update check's refresh
//! and the telemetry send.
//!
//! Detached so it outlives the command and never reaches its terminal. On
//! Windows that is not enough. A child inherits every inheritable handle its
//! parent holds, whatever its own stdio is set to, and the parent's standard
//! handles are inheritable whenever whoever started it made them so: the pipe
//! behind `$(mapbox …)`, PowerShell's `$x = mapbox …`, a test's `output()`.
//! The child would then hold the caller's pipe open, and the caller would wait
//! for it — up to the send's whole budget — before seeing the command finish.
//! So on Windows the parent's own standard handles stop being inheritable
//! first. A later child that asks to inherit stdio still gets it: the standard
//! library hands it a duplicate made inheritable for that child alone.

use std::process::Command;

/// Sets `command` up to run on its own; the caller still chooses its stdio.
pub(crate) fn detach(command: &mut Command) {
    #[cfg(unix)]
    {
        use std::os::unix::process::CommandExt;
        command.process_group(0);
    }
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        const DETACHED_PROCESS: u32 = 0x0000_0008;
        const CREATE_NO_WINDOW: u32 = 0x0800_0000;
        command.creation_flags(DETACHED_PROCESS | CREATE_NO_WINDOW);
        keep_standard_handles_from_children();
    }
}

/// The crate's one `unsafe`: two Win32 calls the standard library has no safe
/// form of. Neither takes a pointer, and the second only clears a flag on a
/// handle this process already owns; a failure leaves the flag as it was,
/// which is today's behavior.
#[cfg(windows)]
#[allow(unsafe_code)]
fn keep_standard_handles_from_children() {
    use windows_sys::Win32::Foundation::{
        SetHandleInformation, HANDLE_FLAG_INHERIT, INVALID_HANDLE_VALUE,
    };
    use windows_sys::Win32::System::Console::{
        GetStdHandle, STD_ERROR_HANDLE, STD_INPUT_HANDLE, STD_OUTPUT_HANDLE,
    };

    for which in [STD_INPUT_HANDLE, STD_OUTPUT_HANDLE, STD_ERROR_HANDLE] {
        // SAFETY: see the function's doc; the handle is checked before use.
        unsafe {
            let handle = GetStdHandle(which);
            if !handle.is_null() && handle != INVALID_HANDLE_VALUE {
                SetHandleInformation(handle, HANDLE_FLAG_INHERIT, 0);
            }
        }
    }
}
