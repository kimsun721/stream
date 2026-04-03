use std::net::UdpSocket;

use crate::utils;

pub fn bind_udp_socket() {
    let host_addr = utils::addr::select_host_address();
    let socket = UdpSocket::bind(format!("{host_addr}:0")).expect("binding a random UDP port");
    let addr = socket.local_addr().expect("a local socket address");
}
