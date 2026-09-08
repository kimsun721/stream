use std::{
    io::ErrorKind,
    net::{Ipv4Addr, SocketAddr, UdpSocket},
    time::Instant,
};

use str0m::{
    Input,
    net::{Protocol, Receive},
};

use crate::{config::tuning, sfu::error::SocketResult, utils};

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

pub fn read_socket_input<'a>(
    socket: &UdpSocket,
    buf: &'a mut Vec<u8>,
) -> SocketResult<Option<Input<'a>>> {
    buf.resize(2000, 0);
    let input = match socket.recv_from(buf) {
        Ok((n, source)) => {
            buf.truncate(n);
            Some(Input::Receive(
                Instant::now(),
                Receive {
                    proto: Protocol::Udp,
                    source,
                    destination: socket.local_addr()?,
                    contents: buf.as_slice().try_into()?,
                },
            ))
        }

        Err(e) => match e.kind() {
            ErrorKind::WouldBlock | ErrorKind::TimedOut => None,
            _ => return Err(e.into()),
        },
    };
    Ok(input)
}
