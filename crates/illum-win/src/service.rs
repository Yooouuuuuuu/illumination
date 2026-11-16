// SPDX-License-Identifier: GPL-3.0-only
//! The installed service and the processes it starts (architecture §3).
//!
//! A program started by the person at the desk cannot type into a window that runs as
//! administrator, nor see the lock screen. A Windows service runs as the system account, which
//! can; but a service lives in session 0, away from every screen. So the service does one
//! thing: it keeps a host process running *inside* the session that has the physical screen,
//! with the service's own system rights. AnyDesk and RustDesk are built the same way.
//!
//! What is here: asking whether the service is installed and running, the service's entry
//! (the dispatcher and its stop signal), and starting a process in a session either with
//! system rights (the host) or as the person signed in there (the window that asks a
//! question). Registering the service is done with `sc.exe` by the caller.

use std::ffi::c_void;
use std::sync::Mutex;
use std::sync::atomic::{AtomicBool, AtomicIsize, Ordering};
use std::time::Duration;
use windows::Win32::Foundation::{
    CloseHandle, ERROR_SERVICE_DOES_NOT_EXIST, HANDLE, WAIT_OBJECT_0,
};
use windows::Win32::Security::{
    DuplicateTokenEx, GetTokenInformation, IsWellKnownSid, SecurityImpersonation,
    SetTokenInformation, TOKEN_ALL_ACCESS, TOKEN_QUERY, TOKEN_USER, TokenPrimary, TokenSessionId,
    TokenUser, WinLocalSystemSid,
};
use windows::Win32::System::Environment::{CreateEnvironmentBlock, DestroyEnvironmentBlock};
use windows::Win32::System::RemoteDesktop::{
    ProcessIdToSessionId, WTSGetActiveConsoleSessionId, WTSQueryUserToken,
};
use windows::Win32::System::Services::{
    CloseServiceHandle, OpenSCManagerW, OpenServiceW, QueryServiceStatus,
    RegisterServiceCtrlHandlerExW, SC_MANAGER_CONNECT, SERVICE_ACCEPT_SHUTDOWN,
    SERVICE_ACCEPT_STOP, SERVICE_CONTROL_SHUTDOWN, SERVICE_CONTROL_STOP, SERVICE_QUERY_STATUS,
    SERVICE_RUNNING, SERVICE_START_PENDING, SERVICE_STATUS, SERVICE_STATUS_CURRENT_STATE,
    SERVICE_STATUS_HANDLE, SERVICE_STOP_PENDING, SERVICE_STOPPED, SERVICE_TABLE_ENTRYW,
    SERVICE_WIN32_OWN_PROCESS, SetServiceStatus, StartServiceCtrlDispatcherW,
};
use windows::Win32::System::Threading::{
    CREATE_NO_WINDOW, CREATE_UNICODE_ENVIRONMENT, CreateProcessAsUserW, GetCurrentProcess,
    GetCurrentProcessId, GetExitCodeProcess, OpenProcessToken, PROCESS_INFORMATION, STARTUPINFOW,
    TerminateProcess, WaitForSingleObject,
};
use windows::core::{PCWSTR, PWSTR, w};

/// The service's name in Windows (`sc query Illumination`).
pub const SERVICE_NAME: &str = "Illumination";
const NAME: PCWSTR = w!("Illumination");

/// What Windows says about the installed service.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ServiceState {
    NotInstalled,
    /// Installed, but not running now (stopped, or starting or stopping).
    Stopped,
    Running,
}

/// Whether the service is installed and running. Anyone may ask.
pub fn service_state() -> ServiceState {
    unsafe {
        let Ok(manager) = OpenSCManagerW(None, None, SC_MANAGER_CONNECT) else {
            return ServiceState::NotInstalled;
        };
        let state = match OpenServiceW(manager, NAME, SERVICE_QUERY_STATUS) {
            Ok(service) => {
                let mut status = SERVICE_STATUS::default();
                let running = QueryServiceStatus(service, &mut status).is_ok()
                    && status.dwCurrentState == SERVICE_RUNNING;
                let _ = CloseServiceHandle(service);
                if running {
                    ServiceState::Running
                } else {
                    ServiceState::Stopped
                }
            }
            Err(e) if e.code() == ERROR_SERVICE_DOES_NOT_EXIST.to_hresult() => {
                ServiceState::NotInstalled
            }
            // Not allowed to look: it exists, and that is all that is known.
            Err(_) => ServiceState::Stopped,
        };
        let _ = CloseServiceHandle(manager);
        state
    }
}

/// What the service does while it runs; it gets the flag that turns true when Windows asks
/// the service to stop.
type Body = Box<dyn FnOnce(&AtomicBool) + Send>;
static BODY: Mutex<Option<Body>> = Mutex::new(None);
static STOP: AtomicBool = AtomicBool::new(false);
static STATUS: AtomicIsize = AtomicIsize::new(0);

fn report(state: SERVICE_STATUS_CURRENT_STATE) {
    let accepted = if state == SERVICE_RUNNING {
        SERVICE_ACCEPT_STOP | SERVICE_ACCEPT_SHUTDOWN
    } else {
        0
    };
    let status = SERVICE_STATUS {
        dwServiceType: SERVICE_WIN32_OWN_PROCESS,
        dwCurrentState: state,
        dwControlsAccepted: accepted,
        dwWaitHint: if state == SERVICE_RUNNING || state == SERVICE_STOPPED {
            0
        } else {
            5000
        },
        ..Default::default()
    };
    let handle = SERVICE_STATUS_HANDLE(STATUS.load(Ordering::SeqCst) as *mut c_void);
    let _ = unsafe { SetServiceStatus(handle, &status) };
}

unsafe extern "system" fn control(control: u32, _: u32, _: *mut c_void, _: *mut c_void) -> u32 {
    if control == SERVICE_CONTROL_STOP || control == SERVICE_CONTROL_SHUTDOWN {
        report(SERVICE_STOP_PENDING);
        STOP.store(true, Ordering::SeqCst);
    }
    0
}

unsafe extern "system" fn service_main(_: u32, _: *mut PWSTR) {
    let Ok(handle) = (unsafe { RegisterServiceCtrlHandlerExW(NAME, Some(control), None) }) else {
        return;
    };
    STATUS.store(handle.0 as isize, Ordering::SeqCst);
    report(SERVICE_START_PENDING);
    let body = BODY.lock().expect("service body").take();
    report(SERVICE_RUNNING);
    if let Some(body) = body {
        body(&STOP);
    }
    report(SERVICE_STOPPED);
}

/// Runs this process as the service: returns when Windows has stopped it. Fails at once when
/// the process was not started by Windows as a service (started by hand, for instance).
pub fn run_as_service(body: impl FnOnce(&AtomicBool) + Send + 'static) -> Result<(), String> {
    *BODY.lock().expect("service body") = Some(Box::new(body));
    let mut name: Vec<u16> = SERVICE_NAME.encode_utf16().chain([0]).collect();
    let table = [
        SERVICE_TABLE_ENTRYW {
            lpServiceName: PWSTR(name.as_mut_ptr()),
            lpServiceProc: Some(service_main),
        },
        SERVICE_TABLE_ENTRYW::default(),
    ];
    unsafe { StartServiceCtrlDispatcherW(table.as_ptr()) }
        .map_err(|e| format!("this is started by Windows as a service, not by hand ({e})"))
}

/// The session that has the physical screen and keyboard now; `None` between two of them.
pub fn console_session() -> Option<u32> {
    let id = unsafe { WTSGetActiveConsoleSessionId() };
    (id != u32::MAX).then_some(id)
}

/// The session this process runs in.
pub fn own_session() -> Option<u32> {
    let mut id = 0u32;
    unsafe { ProcessIdToSessionId(GetCurrentProcessId(), &mut id) }
        .ok()
        .map(|()| id)
}

/// Whether this process runs as the system account (it was started by the service).
pub fn runs_as_system() -> bool {
    unsafe {
        let mut token = HANDLE::default();
        if OpenProcessToken(GetCurrentProcess(), TOKEN_QUERY, &mut token).is_err() {
            return false;
        }
        // TOKEN_USER followed by the SID it points to; 8-aligned storage for the pointer.
        let mut buffer = [0u64; 16];
        let mut size = 0u32;
        let read = GetTokenInformation(
            token,
            TokenUser,
            Some(buffer.as_mut_ptr().cast()),
            std::mem::size_of_val(&buffer) as u32,
            &mut size,
        );
        let _ = CloseHandle(token);
        if read.is_err() {
            return false;
        }
        let user = &*(buffer.as_ptr() as *const TOKEN_USER);
        IsWellKnownSid(user.User.Sid, WinLocalSystemSid).as_bool()
    }
}

/// A process this one started in a session.
pub struct SessionProcess {
    process: HANDLE,
    /// The session it was started in.
    pub session: u32,
}

// The handle is only waited on, asked and terminated: all allowed from any thread.
unsafe impl Send for SessionProcess {}

impl SessionProcess {
    /// Its exit code once it has ended; `None` while it runs.
    pub fn exit_code(&self) -> Option<u32> {
        self.wait(Duration::ZERO)
    }

    /// Waits up to `limit` for it to end.
    pub fn wait(&self, limit: Duration) -> Option<u32> {
        unsafe {
            let ms = limit.as_millis().min(u32::MAX as u128 - 1) as u32;
            if WaitForSingleObject(self.process, ms) != WAIT_OBJECT_0 {
                return None;
            }
            let mut code = 0u32;
            GetExitCodeProcess(self.process, &mut code).ok()?;
            Some(code)
        }
    }

    pub fn kill(&self) {
        let _ = unsafe { TerminateProcess(self.process, 1) };
    }
}

impl Drop for SessionProcess {
    fn drop(&mut self) {
        let _ = unsafe { CloseHandle(self.process) };
    }
}

/// Starts `command_line` in `session` with this process's own rights. Called by the service
/// (the system account), this is the host that can reach every window and the lock screen.
pub fn start_in_session(session: u32, command_line: &str) -> Result<SessionProcess, String> {
    unsafe {
        let mut own = HANDLE::default();
        OpenProcessToken(GetCurrentProcess(), TOKEN_ALL_ACCESS, &mut own)
            .map_err(|e| format!("reading this process's rights: {e}"))?;
        let mut token = HANDLE::default();
        let copied = DuplicateTokenEx(
            own,
            TOKEN_ALL_ACCESS,
            None,
            SecurityImpersonation,
            TokenPrimary,
            &mut token,
        );
        let _ = CloseHandle(own);
        copied.map_err(|e| format!("copying this process's rights: {e}"))?;
        let moved = SetTokenInformation(
            token,
            TokenSessionId,
            (&raw const session).cast(),
            std::mem::size_of::<u32>() as u32,
        );
        let started = match moved {
            Ok(()) => create(token, command_line, None, session),
            Err(e) => Err(format!("moving to session {session}: {e}")),
        };
        let _ = CloseHandle(token);
        started
    }
}

/// Starts `command_line` as the person signed in to `session`, with their environment. Needs
/// the system account; fails when nobody is signed in there.
pub fn start_as_session_user(session: u32, command_line: &str) -> Result<SessionProcess, String> {
    unsafe {
        let mut token = HANDLE::default();
        WTSQueryUserToken(session, &mut token)
            .map_err(|e| format!("nobody is signed in to session {session} ({e})"))?;
        let mut environment: *mut c_void = std::ptr::null_mut();
        let has_environment = CreateEnvironmentBlock(&mut environment, Some(token), false).is_ok();
        let started = create(
            token,
            command_line,
            has_environment.then_some(environment),
            session,
        );
        if has_environment {
            let _ = DestroyEnvironmentBlock(environment);
        }
        let _ = CloseHandle(token);
        started
    }
}

unsafe fn create(
    token: HANDLE,
    command_line: &str,
    environment: Option<*mut c_void>,
    session: u32,
) -> Result<SessionProcess, String> {
    // The desktop people see; without it the process lands on the service's invisible one.
    let mut desktop: Vec<u16> = "winsta0\\default".encode_utf16().chain([0]).collect();
    let mut command: Vec<u16> = command_line.encode_utf16().chain([0]).collect();
    let startup = STARTUPINFOW {
        cb: std::mem::size_of::<STARTUPINFOW>() as u32,
        lpDesktop: PWSTR(desktop.as_mut_ptr()),
        ..Default::default()
    };
    let mut info = PROCESS_INFORMATION::default();
    unsafe {
        CreateProcessAsUserW(
            Some(token),
            None,
            Some(PWSTR(command.as_mut_ptr())),
            None,
            None,
            false,
            CREATE_NO_WINDOW | CREATE_UNICODE_ENVIRONMENT,
            environment.map(|e| e.cast_const()),
            None,
            &startup,
            &mut info,
        )
        .map_err(|e| format!("starting a process in session {session}: {e}"))?;
        let _ = CloseHandle(info.hThread);
    }
    Ok(SessionProcess {
        process: info.hProcess,
        session,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The questions anyone may ask answer without failing, whatever the machine has.
    #[test]
    fn questions_answer() {
        let _ = service_state();
        assert!(own_session().is_some());
        // The tests are not run by the system account.
        assert!(!runs_as_system());
        // Not started by Windows as a service: the dispatcher refuses, and says why.
        let refused = run_as_service(|_| {}).expect_err("not a service");
        assert!(refused.contains("service"), "{refused}");
    }
}
