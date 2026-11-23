//! Cooperates with mhVTL's exclusive directory lock. No lock file enters S3.
use std::{
    fs::{File, OpenOptions},
    io,
    os::unix::fs::OpenOptionsExt,
    path::Path,
};

#[derive(Debug)]
pub struct MediaReadGuard {
    _directory: File,
}

impl MediaReadGuard {
    pub fn acquire(path: &Path) -> io::Result<Self> {
        let directory = OpenOptions::new()
            .read(true)
            .custom_flags(libc::O_DIRECTORY | libc::O_NOFOLLOW | libc::O_CLOEXEC)
            .open(path)?;
        directory.try_lock_shared().map_err(|error| match error {
            std::fs::TryLockError::WouldBlock => io::Error::new(io::ErrorKind::WouldBlock,
                "A drive is using this cartridge. Unload it in PBS before sending; other cartridges can remain in service."),
            std::fs::TryLockError::Error(error) => error,
        })?;
        Ok(Self {
            _directory: directory,
        })
    }
}
