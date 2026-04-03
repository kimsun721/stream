use std::{io::ErrorKind, net::UdpSocket, sync::mpsc::Receiver, time::Instant};

use str0m::{
    Event, IceConnectionState, Input, Output, Rtc, RtcError,
    net::{Protocol, Receive},
};

use crate::types::{ClientRole, RoomId};

pub fn run(rx: Receiver<(Rtc, ClientRole, RoomId)>, socket: UdpSocket) -> Result<(), RtcError> {
    let mut buf = Vec::new();
    let (mut rtc, role, room_id) = rx.recv().expect("dk");

    loop {
        let timeout = match rtc.poll_output()? {
            Output::Timeout(v) => v,

            Output::Transmit(v) => {
                socket.send_to(&v.contents, v.destination)?;
                continue;
            }

            Output::Event(v) => {
                if v == Event::IceConnectionStateChange(IceConnectionState::Disconnected) {
                    return Ok(());
                }
                continue;
            }
        };

        let timeout = timeout - Instant::now();

        // socket.set_read_timeout(Some(0)) is not ok
        if timeout.is_zero() {
            rtc.handle_input(Input::Timeout(Instant::now()))?;
            continue;
        }

        socket.set_read_timeout(Some(timeout))?;
        buf.resize(2000, 0);

        let input = match socket.recv_from(&mut buf) {
            Ok((n, source)) => {
                buf.truncate(n);
                Input::Receive(
                    Instant::now(),
                    Receive {
                        proto: Protocol::Udp,
                        source,
                        destination: socket.local_addr().unwrap(),
                        contents: buf.as_slice().try_into()?,
                    },
                )
            }

            Err(e) => match e.kind() {
                // Expected error for set_read_timeout(). One for windows, one for the rest.
                ErrorKind::WouldBlock | ErrorKind::TimedOut => Input::Timeout(Instant::now()),
                _ => return Err(e.into()),
            },
        };

        rtc.handle_input(input)?;
    }
}
