use thiserror::Error;

#[derive(Debug, Error)]
pub enum ClientError {
    #[error("str0m: {0}")]
    Rtc(#[from] str0m::RtcError),

    #[error("io: {0}")]
    Io(#[from] std::io::Error),

    #[error("serde: {0}")]
    Serde(#[from] serde_json::Error),
}

pub type ClientResult<T> = Result<T, ClientError>;

#[derive(Debug, Error)]
pub enum SocketError {
    #[error("io: {0}")]
    Io(#[from] std::io::Error),

    #[error("custom: {0}")]
    Net(#[from] str0m::error::NetError),
}

pub type SocketResult<T> = Result<T, SocketError>;
