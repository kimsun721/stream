use std::{
    net::{SocketAddr, UdpSocket},
    sync::{
        atomic::{AtomicUsize, Ordering},
        mpsc::{SyncSender, sync_channel},
    },
};

use tracing::error;

use crate::{
    config::tuning,
    sfu,
    types::{RoomId, SfuMessage},
};

pub struct Shard {
    pub sender: SyncSender<SfuMessage>,
}

pub struct Shards {
    pub next_shard: AtomicUsize,
    pub shards: Vec<Shard>,
    pub advertised_addr: SocketAddr,
}

impl Shards {
    pub fn next_shard(&self) -> usize {
        self.next_shard.fetch_add(1, Ordering::Relaxed) % self.shards.len()
    }

    pub fn sender_for(&self, room_id: &RoomId) -> Option<&SyncSender<SfuMessage>> {
        let shard_id = room_id.0.split("_").nth(1)?.parse::<usize>().ok();

        if let Some(shard_id) = shard_id {
            self.shards.get(shard_id).map(|s| &s.sender)
        } else {
            None
        }
    }

    pub fn senders(&self) -> Vec<&SyncSender<SfuMessage>> {
        self.shards.iter().map(|s| &s.sender).collect()
    }

    pub fn new(socket: UdpSocket, advertised_addr: SocketAddr) -> Self {
        let mut shards = Shards {
            next_shard: AtomicUsize::new(0),
            shards: Vec::new(),
            advertised_addr,
        };

        for i in 0..tuning().server.total_shards.get() {
            let (sfu_tx, sfu_rx) = sync_channel::<SfuMessage>(2048);
            let socket = socket.try_clone().expect("failed to clone socket");

            shards.shards.insert(i, Shard { sender: sfu_tx });

            std::thread::spawn(move || {
                if let Err(e) = sfu::run::run(i, sfu_rx, socket, advertised_addr) {
                    error!("udp error : {e}");
                };
            });
        }

        shards
    }
}
