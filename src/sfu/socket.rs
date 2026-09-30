use std::net::{Ipv4Addr, SocketAddr, UdpSocket};

use crate::{config::tuning, utils};

pub fn bind_udp_socket() -> (SocketAddr, UdpSocket) {
    let port = tuning().server.media_port;

    let socket = UdpSocket::bind(SocketAddr::new(Ipv4Addr::UNSPECIFIED.into(), port))
        .unwrap_or_else(|e| panic!("binding UDP port {port}: {e}"));

    let public_ip = tuning()
        .server
        .public_ip
        .unwrap_or_else(utils::addr::select_host_address);

    (SocketAddr::new(public_ip, port), socket)
}
