//! The server token lives in Windows Credential Manager as a generic credential.
//! Like the Mac client's Keychain entry, it is scoped to the data directory and
//! the endpoint, so one server's token is never sent to another.

use crate::config::data_dir;
use crate::win::wide;
use std::ptr::{null, null_mut};
use windows_sys::Win32::Foundation::{ERROR_NOT_FOUND, GetLastError};
use windows_sys::Win32::Security::Credentials::{
    CRED_PERSIST_LOCAL_MACHINE, CRED_TYPE_GENERIC, CREDENTIALW, CredDeleteW, CredFree, CredReadW,
    CredWriteW,
};

fn target(endpoint: &str) -> Vec<u16> {
    wide(&format!("Inlay|{}|{endpoint}", data_dir().display()))
}

pub fn read(endpoint: &str) -> String {
    let target = target(endpoint);
    let mut credential: *mut CREDENTIALW = null_mut();
    unsafe {
        if CredReadW(target.as_ptr(), CRED_TYPE_GENERIC, 0, &mut credential) == 0 {
            return String::new();
        }
        let size = (*credential).CredentialBlobSize as usize;
        let token = if size == 0 {
            String::new()
        } else {
            String::from_utf8_lossy(std::slice::from_raw_parts(
                (*credential).CredentialBlob,
                size,
            ))
            .into_owned()
        };
        CredFree(credential.cast());
        token
    }
}

/// Saves `token` for `endpoint`. An empty token removes the credential.
pub fn write(endpoint: &str, token: &str) -> Result<(), String> {
    let target = target(endpoint);
    unsafe {
        if token.is_empty() {
            if CredDeleteW(target.as_ptr(), CRED_TYPE_GENERIC, 0) == 0
                && GetLastError() != ERROR_NOT_FOUND
            {
                return Err(format!(
                    "Windows could not remove the server token (error {}).",
                    GetLastError()
                ));
            }
            return Ok(());
        }
        let user = wide("Inlay");
        let mut blob = token.as_bytes().to_vec();
        let credential = CREDENTIALW {
            Flags: 0,
            Type: CRED_TYPE_GENERIC,
            TargetName: target.as_ptr().cast_mut(),
            Comment: null_mut(),
            LastWritten: std::mem::zeroed(),
            CredentialBlobSize: blob.len() as u32,
            CredentialBlob: blob.as_mut_ptr(),
            Persist: CRED_PERSIST_LOCAL_MACHINE,
            AttributeCount: 0,
            Attributes: null_mut(),
            TargetAlias: null::<u16>().cast_mut(),
            UserName: user.as_ptr().cast_mut(),
        };
        if CredWriteW(&credential, 0) == 0 {
            return Err(format!(
                "Windows could not save the server token (error {}).",
                GetLastError()
            ));
        }
    }
    Ok(())
}
