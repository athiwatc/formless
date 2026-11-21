use anyhow::{ensure, Result};
use std::{
    fs::{File, OpenOptions},
    os::windows::{
        fs::{MetadataExt, OpenOptionsExt},
        io::AsRawHandle,
    },
    path::Path,
};
use windows_sys::Win32::{
    Foundation::LocalFree,
    Security::{
        Authorization::{GetSecurityInfo, SetSecurityInfo, SE_FILE_OBJECT},
        GetSecurityDescriptorControl, DACL_SECURITY_INFORMATION,
        PROTECTED_DACL_SECURITY_INFORMATION, SE_DACL_PROTECTED,
        UNPROTECTED_DACL_SECURITY_INFORMATION,
    },
    Storage::FileSystem::{
        FILE_ATTRIBUTE_REPARSE_POINT, FILE_FLAG_OPEN_REPARSE_POINT, FILE_READ_ATTRIBUTES,
        FILE_SHARE_DELETE, FILE_SHARE_READ, FILE_SHARE_WRITE, READ_CONTROL,
    },
};

pub(super) fn copy_dacl(destination: &Path, temporary: &File) -> Result<()> {
    let existing = OpenOptions::new()
        .access_mode(READ_CONTROL | FILE_READ_ATTRIBUTES)
        .share_mode(FILE_SHARE_READ | FILE_SHARE_WRITE | FILE_SHARE_DELETE)
        .custom_flags(FILE_FLAG_OPEN_REPARSE_POINT)
        .open(destination)?;
    let metadata = existing.metadata()?;
    ensure!(
        metadata.is_file() && metadata.file_attributes() & FILE_ATTRIBUTE_REPARSE_POINT == 0,
        "output destination must remain a regular file"
    );
    let mut acl = std::ptr::null_mut();
    let mut descriptor = std::ptr::null_mut();
    // SAFETY: the handle is live and both pointers are initialized outputs.
    let status = unsafe {
        GetSecurityInfo(
            existing.as_raw_handle(),
            SE_FILE_OBJECT,
            DACL_SECURITY_INFORMATION,
            std::ptr::null_mut(),
            std::ptr::null_mut(),
            &mut acl,
            std::ptr::null_mut(),
            &mut descriptor,
        )
    };
    ensure!(
        status == 0,
        std::io::Error::from_raw_os_error(status as i32)
    );
    struct Descriptor(*mut std::ffi::c_void);
    impl Drop for Descriptor {
        fn drop(&mut self) {
            // SAFETY: GetSecurityInfo transfers this native allocation.
            unsafe { LocalFree(self.0) };
        }
    }
    let _descriptor = Descriptor(descriptor);
    let mut control = 0;
    let mut revision = 0;
    // SAFETY: the descriptor is owned above and both outputs have the
    // native types inferred from the function signature.
    ensure!(
        unsafe { GetSecurityDescriptorControl(descriptor, &mut control, &mut revision) } != 0,
        std::io::Error::last_os_error()
    );
    let protection = if control & SE_DACL_PROTECTED != 0 {
        PROTECTED_DACL_SECURITY_INFORMATION
    } else {
        UNPROTECTED_DACL_SECURITY_INFORMATION
    };
    // SAFETY: the temporary handle has WRITE_DAC and the borrowed ACL stays
    // valid until the owned security descriptor is released.
    let status = unsafe {
        SetSecurityInfo(
            temporary.as_raw_handle(),
            SE_FILE_OBJECT,
            DACL_SECURITY_INFORMATION | protection,
            std::ptr::null_mut(),
            std::ptr::null_mut(),
            acl,
            std::ptr::null(),
        )
    };
    ensure!(
        status == 0,
        std::io::Error::from_raw_os_error(status as i32)
    );
    Ok(())
}
