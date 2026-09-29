use std::{
    io::ErrorKind,
    net::UdpSocket,
    sync::{Arc, mpsc::TrySendError},
};

use tracing::error;

use crate::{
    metrics,
    mux::error::{MuxError::ChannelDisconnected, MuxResult},
    shards::types::Shards,
    types::SfuMessage,
};

pub fn run(shards: Arc<Shards>, socket: UdpSocket) -> MuxResult<()> {
    let mut buf: Vec<u8> = vec![0; 2000];

    socket.set_nonblocking(false)?;

    loop {
        match socket.recv_from(&mut buf) {
            Ok((n, source)) => {
                metrics::socket_read(n);

                for tx in shards.senders() {
                    let data = buf[..n].to_vec();

                    match tx.try_send(SfuMessage::Datagram { data, source }) {
                        Ok(()) => {}
                        Err(TrySendError::Full(_)) => metrics::channel_dropped(),
                        Err(TrySendError::Disconnected(_)) => return Err(ChannelDisconnected),
                    }
                }
            }

            Err(e) => match e.kind() {
                ErrorKind::WouldBlock | ErrorKind::TimedOut => {}
                _ => {
                    error!("socket recv failed error={e}");
                }
            },
        }
    }
}
