// SPDX-License-Identifier: GPL-3.0-only
//! Ctrl+Alt+Del for a controller (architecture §7).
//!
//! Windows takes that key combination from nobody but the keyboard: it cannot be injected
//! like other keys. A service running as the system may ask for the same screen with
//! `SendSAS`, if the machine's policy allows services to (`SoftwareSASGeneration`, which
//! installing sets). The host that serves sessions is not that service; it runs in the
//! person's session. So the host raises a flag ([`request`]) and the service, which waits
//! on it ([`SasGate`]), makes the call.

use std::time::Duration;
use windows::Win32::Foundation::{CloseHandle, HANDLE, WAIT_OBJECT_0};
use windows::Win32::System::LibraryLoader::{GetProcAddress, LoadLibraryW};
use windows::Win32::System::Threading::{
    CreateEventW, EVENT_MODIFY_STATE, OpenEventW, SetEvent, WaitForSingleObject,
};
use windows::core::{s, w};

/// Seen by every session of the machine; made by the service, so only the system (and
/// administrators) can open it.
const FLAG: windows::core::PCWSTR = w!("Global\\Illumination-SAS");

/// The service's end: made once, waited on in its loop.
pub struct SasGate {
    flag: HANDLE,
}

// A handle to an event, used from the service's one thread.
unsafe impl Send for SasGate {}

impl SasGate {
    pub fn open() -> Result<SasGate, String> {
        // Auto-reset, not raised.
        let flag = unsafe { CreateEventW(None, false, false, FLAG) }
            .map_err(|e| format!("making the Ctrl+Alt+Del flag: {e}"))?;
        Ok(SasGate { flag })
    }

    /// Waits up to `limit` for a host to ask. Returns whether one did.
    pub fn asked(&self, limit: Duration) -> bool {
        unsafe { WaitForSingleObject(self.flag, limit.as_millis() as u32) == WAIT_OBJECT_0 }
    }
}

impl Drop for SasGate {
    fn drop(&mut self) {
        let _ = unsafe { CloseHandle(self.flag) };
    }
}

/// The host's end: asks the service for the Ctrl+Alt+Del screen. Fails when no service of
/// ours is running, or this program is not allowed to ask it (it is not the system).
pub fn request() -> Result<(), String> {
    unsafe {
        let flag = OpenEventW(EVENT_MODIFY_STATE, false, FLAG)
            .map_err(|e| format!("the Illumination service is not there to ask ({e})"))?;
        let raised = SetEvent(flag);
        let _ = CloseHandle(flag);
        raised.map_err(|e| format!("asking the Illumination service: {e}"))
    }
}

/// Asks Windows for the Ctrl+Alt+Del screen. Only works in a service that runs as the
/// system, on a machine whose policy lets services do it; otherwise Windows does nothing
/// and reports nothing.
pub fn send() -> Result<(), String> {
    unsafe {
        let library =
            LoadLibraryW(w!("sas.dll")).map_err(|e| format!("sas.dll is not available: {e}"))?;
        let send_sas = GetProcAddress(library, s!("SendSAS"))
            .ok_or_else(|| "sas.dll has no SendSAS".to_string())?;
        // void SendSAS(BOOL AsUser); FALSE: the caller is a service.
        let send_sas: unsafe extern "system" fn(i32) = std::mem::transmute(send_sas);
        send_sas(0);
        Ok(())
    }
}

/// Locks the machine, as Win+L does. Works for a program in the session at the screen.
pub fn lock_workstation() -> Result<(), String> {
    unsafe { windows::Win32::System::Shutdown::LockWorkStation() }
        .map_err(|e| format!("Windows did not lock: {e}"))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The flag carries one request from a host to the service and is then down again.
    /// (As an ordinary program the flag cannot be made in `Global\`; the test then has
    /// nothing to check, and says so.)
    #[test]
    fn a_request_reaches_the_gate_once() {
        let gate = match SasGate::open() {
            Ok(gate) => gate,
            Err(why) => {
                eprintln!("no gate for this test ({why}); skipped");
                return;
            }
        };
        assert!(!gate.asked(Duration::ZERO), "nothing asked yet");
        request().expect("the gate is open");
        assert!(gate.asked(Duration::from_millis(200)));
        assert!(!gate.asked(Duration::ZERO), "one request, one answer");
    }
}
