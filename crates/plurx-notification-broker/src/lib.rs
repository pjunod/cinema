#![forbid(unsafe_code)]
pub mod config;
pub mod http;
pub mod listener;
pub mod provider;
pub mod store;

pub mod wire;

#[derive(Clone, Debug)]
pub struct Error {
    pub status: u16,
    pub code: &'static str,
}
impl Error {
    pub fn new(status: u16, code: &'static str) -> Self {
        Self { status, code }
    }
    pub fn invalid() -> Self {
        Self::new(400, "invalid")
    }
    pub fn unavailable() -> Self {
        Self::new(503, "unavailable")
    }
}
impl std::fmt::Display for Error {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(self.code)
    }
}
impl std::error::Error for Error {}
pub type Result<T> = std::result::Result<T, Error>;
