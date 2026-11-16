// SPDX-License-Identifier: GPL-3.0-only
//! Who may talk to the installed host on its local pipe (architecture §3): the system and
//! the person signed in at the screen, nobody else.

use std::ffi::c_void;
use windows::Win32::Foundation::{CloseHandle, HANDLE, HLOCAL, LocalFree};
use windows::Win32::Security::Authorization::{
    ConvertSidToStringSidW, ConvertStringSecurityDescriptorToSecurityDescriptorW, SDDL_REVISION_1,
};
use windows::Win32::Security::{
    GetTokenInformation, PSECURITY_DESCRIPTOR, SECURITY_ATTRIBUTES, TOKEN_QUERY, TOKEN_USER,
    TokenUser,
};
use windows::Win32::System::Pipes::GetNamedPipeClientSessionId;
use windows::Win32::System::RemoteDesktop::WTSQueryUserToken;
use windows::Win32::System::Threading::{GetCurrentProcess, OpenProcessToken};
use windows::core::{HSTRING, PWSTR};

/// The person signed in to `session`, as Windows writes an account (`S-1-5-21-…`). Needs
/// the system account; `None` when nobody is signed in there.
pub fn session_user_sid(session: u32) -> Option<String> {
    let mut token = HANDLE::default();
    unsafe { WTSQueryUserToken(session, &mut token) }.ok()?;
    account_of(token)
}

/// The account this program runs as.
pub fn own_user_sid() -> Option<String> {
    let mut token = HANDLE::default();
    unsafe { OpenProcessToken(GetCurrentProcess(), TOKEN_QUERY, &mut token) }.ok()?;
    account_of(token)
}

/// The account a token stands for; closes the token.
fn account_of(token: HANDLE) -> Option<String> {
    unsafe {
        // TOKEN_USER followed by the account itself; 256 bytes hold the longest there is.
        let mut buffer = [0u64; 32];
        let mut needed = 0u32;
        let read = GetTokenInformation(
            token,
            TokenUser,
            Some(buffer.as_mut_ptr().cast()),
            std::mem::size_of_val(&buffer) as u32,
            &mut needed,
        );
        let _ = CloseHandle(token);
        read.ok()?;
        let user = &*(buffer.as_ptr().cast::<TOKEN_USER>());
        let mut text = PWSTR::null();
        ConvertSidToStringSidW(user.User.Sid, &mut text).ok()?;
        let sid = text.to_string().ok();
        let _ = LocalFree(Some(HLOCAL(text.0.cast())));
        sid
    }
}

/// The access list of the host's pipe, in the form `CreateNamedPipe` takes.
pub struct PipeSecurity {
    descriptor: PSECURITY_DESCRIPTOR,
    attributes: SECURITY_ATTRIBUTES,
}

// Plain memory owned by this value.
unsafe impl Send for PipeSecurity {}

impl PipeSecurity {
    /// The system, and `user` (an account as [`session_user_sid`] gives it) if there is one.
    pub fn system_and(user: Option<&str>) -> Option<PipeSecurity> {
        let mut sddl = "D:P(A;;GA;;;SY)".to_string();
        if let Some(user) = user {
            // Only what an account looks like goes into the list.
            if !user.starts_with("S-1-")
                || !user.chars().all(|c| c.is_ascii_alphanumeric() || c == '-')
            {
                return None;
            }
            sddl.push_str(&format!("(A;;GRGW;;;{user})"));
        }
        let mut descriptor = PSECURITY_DESCRIPTOR::default();
        unsafe {
            ConvertStringSecurityDescriptorToSecurityDescriptorW(
                &HSTRING::from(sddl),
                SDDL_REVISION_1,
                &mut descriptor,
                None,
            )
        }
        .ok()?;
        Some(PipeSecurity {
            descriptor,
            attributes: SECURITY_ATTRIBUTES {
                nLength: std::mem::size_of::<SECURITY_ATTRIBUTES>() as u32,
                lpSecurityDescriptor: descriptor.0,
                bInheritHandle: false.into(),
            },
        })
    }

    /// A `SECURITY_ATTRIBUTES*`, good for as long as this value lives.
    pub fn as_ptr(&mut self) -> *mut c_void {
        self.attributes.lpSecurityDescriptor = self.descriptor.0;
        (&raw mut self.attributes).cast()
    }
}

impl Drop for PipeSecurity {
    fn drop(&mut self) {
        let _ = unsafe { LocalFree(Some(HLOCAL(self.descriptor.0))) };
    }
}

/// The session of the program at the other end of a pipe (`pipe`: the server's handle).
pub fn pipe_client_session(pipe: *mut c_void) -> Option<u32> {
    let mut session = 0u32;
    unsafe { GetNamedPipeClientSessionId(HANDLE(pipe), &mut session) }.ok()?;
    Some(session)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_access_list_takes_accounts_only() {
        assert!(PipeSecurity::system_and(None).is_some());
        let mut with_user = PipeSecurity::system_and(Some("S-1-5-21-1-2-3-1001")).unwrap();
        assert!(!with_user.as_ptr().is_null());
        assert!(PipeSecurity::system_and(Some("S-1-5-21)(A;;GA;;;WD")).is_none());
        assert!(PipeSecurity::system_and(Some("everyone")).is_none());
        let me = own_user_sid().expect("this account");
        assert!(me.starts_with("S-1-5-"), "{me}");
        assert!(PipeSecurity::system_and(Some(&me)).is_some());
    }
}
