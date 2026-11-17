//! Current-user named-pipe ACL construction and client token validation.

#[cfg(windows)]
use windows::Win32::Foundation::{CloseHandle, LocalFree, HANDLE};
#[cfg(windows)]
use windows::Win32::Security::Authorization::{
    ConvertStringSecurityDescriptorToSecurityDescriptorW, SDDL_REVISION_1,
};
#[cfg(windows)]
use windows::Win32::Security::{
    GetTokenInformation, TokenUser, SECURITY_ATTRIBUTES, TOKEN_QUERY, TOKEN_USER,
};
#[cfg(windows)]
use windows::Win32::System::Threading::{GetCurrentProcess, OpenProcessToken};

#[cfg(windows)]
pub struct CurrentUserPipeSecurity {
    attributes: SECURITY_ATTRIBUTES,
    descriptor: windows::Win32::Security::PSECURITY_DESCRIPTOR,
}

#[cfg(windows)]
impl CurrentUserPipeSecurity {
    pub fn new() -> std::io::Result<Self> {
        let sid = current_user_sid()?;
        let sddl = format!("D:P(A;;GA;;;{sid})");
        let wide: Vec<u16> = sddl.encode_utf16().chain(std::iter::once(0)).collect();
        let mut descriptor = windows::Win32::Security::PSECURITY_DESCRIPTOR::default();
        unsafe {
            ConvertStringSecurityDescriptorToSecurityDescriptorW(
                windows::core::PCWSTR(wide.as_ptr()),
                SDDL_REVISION_1,
                &mut descriptor,
                None,
            )
            .map_err(io_error)?;
        }
        let attributes = SECURITY_ATTRIBUTES {
            nLength: std::mem::size_of::<SECURITY_ATTRIBUTES>() as u32,
            lpSecurityDescriptor: descriptor.0,
            bInheritHandle: false.into(),
        };
        Ok(Self {
            attributes,
            descriptor,
        })
    }

    pub fn attributes(&self) -> *mut std::ffi::c_void {
        &self.attributes as *const SECURITY_ATTRIBUTES as *mut std::ffi::c_void
    }
}

#[cfg(windows)]
impl Drop for CurrentUserPipeSecurity {
    fn drop(&mut self) {
        unsafe {
            let _ = LocalFree(windows::Win32::Foundation::HLOCAL(self.descriptor.0));
        }
    }
}

#[cfg(windows)]
pub fn validate_pipe_client(pipe: HANDLE) -> std::io::Result<()> {
    use windows::Win32::Security::RevertToSelf;
    use windows::Win32::System::Pipes::{GetNamedPipeClientSessionId, ImpersonateNamedPipeClient};
    use windows::Win32::System::Threading::{GetCurrentThread, OpenThreadToken};

    let mut client_session = 0;
    unsafe { GetNamedPipeClientSessionId(pipe, &mut client_session).map_err(io_error)? };
    if client_session != current_session_id()? {
        return Err(std::io::Error::new(
            std::io::ErrorKind::PermissionDenied,
            "named-pipe client belongs to a different Windows session",
        ));
    }

    unsafe { ImpersonateNamedPipeClient(pipe).map_err(io_error)? };
    let mut token = HANDLE::default();
    let open_result = unsafe {
        OpenThreadToken(GetCurrentThread(), TOKEN_QUERY, true, &mut token).map_err(io_error)
    };
    let sid_result = open_result.and_then(|()| sid_for_token(token));
    unsafe {
        if !token.is_invalid() && !token.0.is_null() {
            let _ = CloseHandle(token);
        }
    }
    let revert_result = unsafe { RevertToSelf().map_err(io_error) };
    let client_sid = sid_result?;
    revert_result?;
    if client_sid != current_user_sid()? {
        return Err(std::io::Error::new(
            std::io::ErrorKind::PermissionDenied,
            "named-pipe client belongs to a different Windows user",
        ));
    }
    Ok(())
}

#[cfg(windows)]
fn current_session_id() -> std::io::Result<u32> {
    use windows::Win32::System::Threading::GetCurrentProcessId;
    #[link(name = "kernel32")]
    extern "system" {
        fn ProcessIdToSessionId(
            dwProcessId: u32,
            pSessionId: *mut u32,
        ) -> windows::Win32::Foundation::BOOL;
    }
    let mut session = 0;
    let res = unsafe { ProcessIdToSessionId(GetCurrentProcessId(), &mut session) };
    if res.as_bool() {
        Ok(session)
    } else {
        Err(std::io::Error::last_os_error())
    }
}

#[cfg(windows)]
fn current_user_sid() -> std::io::Result<String> {
    let token = token_for_process(unsafe { GetCurrentProcess() })?;
    let result = sid_for_token(token);
    unsafe {
        let _ = CloseHandle(token);
    }
    result
}

#[cfg(windows)]
fn token_for_process(process: HANDLE) -> std::io::Result<HANDLE> {
    let mut token = HANDLE::default();
    unsafe { OpenProcessToken(process, TOKEN_QUERY, &mut token).map_err(io_error)? };
    Ok(token)
}

#[cfg(windows)]
fn sid_for_token(token: HANDLE) -> std::io::Result<String> {
    use windows::core::PWSTR;
    use windows::Win32::Security::Authorization::ConvertSidToStringSidW;

    let mut required = 0u32;
    let _ = unsafe { GetTokenInformation(token, TokenUser, None, 0, &mut required) };
    if required == 0 {
        return Err(std::io::Error::last_os_error());
    }
    let mut buffer = vec![0u64; (required as usize).div_ceil(std::mem::size_of::<u64>())];
    unsafe {
        GetTokenInformation(
            token,
            TokenUser,
            Some(buffer.as_mut_ptr().cast()),
            required,
            &mut required,
        )
        .map_err(io_error)?;
    }
    let token_user = unsafe { &*buffer.as_ptr().cast::<TOKEN_USER>() };
    let mut sid = PWSTR::null();
    unsafe { ConvertSidToStringSidW(token_user.User.Sid, &mut sid).map_err(io_error)? };
    let value = unsafe { sid.to_string() }.map_err(|e| std::io::Error::other(e.to_string()))?;
    unsafe {
        let _ = LocalFree(windows::Win32::Foundation::HLOCAL(sid.0.cast()));
    }
    Ok(value)
}

#[cfg(windows)]
fn io_error(error: windows::core::Error) -> std::io::Error {
    std::io::Error::other(error.to_string())
}
