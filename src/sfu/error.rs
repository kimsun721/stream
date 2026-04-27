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

    #[error("str0m: {0}")]
    Net(#[from] str0m::error::NetError),
}

pub type SocketResult<T> = Result<T, SocketError>;

#[derive(Debug, Error)]
pub enum SfuError {
    #[error("client: {0}")]
    Client(#[from] ClientError),

    #[error("socket: {0}")]
    Socket(#[from] SocketError),
}

pub type SfuResult<T> = Result<T, SfuError>;
