use std::{
    collections::{HashMap, hash_map::Entry},
    io::ErrorKind,
    net::{SocketAddr, UdpSocket},
    sync::{
        Arc,
        mpsc::{Receiver, SyncSender, TrySendError},
    },
    time::Instant,
};

use tracing::error;

use crate::{
    metrics,
    mux::error::{MuxError::ChannelDisconnected, MuxResult},
    shards::types::{ShardRouteMsg, Shards},
    types::SfuMessage,
};

pub fn run(
    shards: Arc<Shards>,
    socket: UdpSocket,
    shard_route_rx: Receiver<ShardRouteMsg>,
) -> MuxResult<()> {
    let mut buf: Vec<u8> = vec![0; 2000];

    socket.set_nonblocking(false)?;

    let mut shard_routes: HashMap<SocketAddr, usize> = HashMap::new();

    loop {
        for msg in shard_route_rx.try_iter().take(500) {
            match msg {
                ShardRouteMsg::Owned { source, shard } => {
                    shard_routes.insert(source, shard);
                }
                ShardRouteMsg::Disowned { source, shard } => match shard_routes.entry(source) {
                    Entry::Occupied(e) if *e.get() == shard => {
                        e.remove();
                    }
                    _ => (),
                },
            };
        }

        match socket.recv_from(&mut buf) {
            Ok((n, source)) => {
                let received_at = Instant::now();
                metrics::socket_read(n);

                if let Some(idx) = shard_routes.get(&source)
                    && let Some(shard) = shards.shards.get(*idx)
                {
                    let data = buf[..n].to_vec();

                    send(&shard.sender, data, source, received_at)?;
                    continue;
                }

                for tx in shards.senders() {
                    let data = buf[..n].to_vec();

                    send(tx, data, source, received_at)?;
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

fn send(
    tx: &SyncSender<SfuMessage>,
    data: Vec<u8>,
    source: SocketAddr,
    received_at: Instant,
) -> MuxResult<()> {
    match tx.try_send(SfuMessage::Datagram {
        data,
        source,
        received_at,
    }) {
        Ok(()) => Ok(()),
        Err(TrySendError::Full(_)) => {
            metrics::channel_dropped();
            Ok(())
        }
        Err(TrySendError::Disconnected(_)) => Err(ChannelDisconnected),
    }
}
