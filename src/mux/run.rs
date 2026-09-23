use std::{
    io::ErrorKind,
    net::UdpSocket,
    sync::mpsc::{SyncSender, TrySendError},
};

use tracing::error;

use crate::{
    metrics,
    mux::error::{MuxError::ChannelDisconnected, MuxResult},
    types::SfuMessage,
};

pub fn run(tx: SyncSender<SfuMessage>, socket: UdpSocket) -> MuxResult<()> {
    let mut buf: Vec<u8> = vec![0; 2000];

    socket.set_nonblocking(false)?;

    loop {
        match socket.recv_from(&mut buf) {
            Ok((n, source)) => {
                metrics::socket_read(n);

                let data = buf[..n].to_vec();

                match tx.try_send(SfuMessage::Datagram { data, source }) {
                    Ok(()) => {}
                    Err(TrySendError::Full(_)) => metrics::channel_dropped(),
                    Err(TrySendError::Disconnected(_)) => return Err(ChannelDisconnected),
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
