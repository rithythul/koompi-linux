//! One error type. A message that names the file and the key beats a taxonomy
//! of variants nobody matches on.

use std::fmt;

#[derive(Debug)]
pub struct Error(String);

pub type Result<T> = std::result::Result<T, Error>;

impl Error {
    pub fn new(msg: impl Into<String>) -> Self {
        Error(msg.into())
    }

    /// Prefix context onto an existing error, innermost cause last.
    pub fn ctx(self, prefix: impl fmt::Display) -> Self {
        Error(format!("{prefix}: {}", self.0))
    }
}

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

impl std::error::Error for Error {}

impl From<std::io::Error> for Error {
    fn from(e: std::io::Error) -> Self {
        Error(e.to_string())
    }
}

/// `bail!("...")` — build an `Err(Error)` with a formatted message.
macro_rules! bail {
    ($($arg:tt)*) => { return Err($crate::err::Error::new(format!($($arg)*))) };
}
pub(crate) use bail;
