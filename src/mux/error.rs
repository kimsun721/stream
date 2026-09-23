use thiserror::Error;

#[derive(Debug, Error)]
pub enum MuxError {
    #[error("io: {0}")]
    Io(#[from] std::io::Error),

    #[error("channel disconnected")]
    ChannelDisconnected,
}

pub type MuxResult<T> = Result<T, MuxError>;
