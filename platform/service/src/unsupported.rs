use crate::{Error, Spec};

pub(crate) fn install(_spec: &Spec) -> Result<String, Error> {
    Err(Error::Unsupported)
}

pub(crate) fn uninstall() -> Result<String, Error> {
    Err(Error::Unsupported)
}

pub(crate) fn status() -> Result<String, Error> {
    Err(Error::Unsupported)
}

pub(crate) fn stop() -> Result<String, Error> {
    Err(Error::Unsupported)
}
