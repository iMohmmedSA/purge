use std::{
    fs::{self, File, TryLockError},
    io,
    path::Path,
};

/// Held for the duration of a run, so the daemon and a manual `purge run`
/// never clean at the same time. Released on drop.
#[derive(Debug)]
pub struct RunLock {
    _file: File,
}

impl RunLock {
    /// Returns `None` if another process holds the lock.
    pub fn try_acquire(path: &Path) -> io::Result<Option<Self>> {
        if let Some(dir) = path.parent() {
            fs::create_dir_all(dir)?;
        }
        let file = File::options()
            .create(true)
            .truncate(false)
            .write(true)
            .open(path)?;
        match file.try_lock() {
            Ok(()) => Ok(Some(Self { _file: file })),
            Err(TryLockError::WouldBlock) => Ok(None),
            Err(TryLockError::Error(err)) => Err(err),
        }
    }

    /// Whether some process currently holds the lock.
    pub fn is_held(path: &Path) -> io::Result<bool> {
        if !path.exists() {
            return Ok(false);
        }
        Ok(Self::try_acquire(path)?.is_none())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn second_acquire_fails_until_release() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("purge.lock");
        let first = RunLock::try_acquire(&path).unwrap();
        assert!(first.is_some());
        assert!(RunLock::try_acquire(&path).unwrap().is_none());
        assert!(RunLock::is_held(&path).unwrap());
        drop(first);
        assert!(RunLock::try_acquire(&path).unwrap().is_some());
    }
}
