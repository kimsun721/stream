use std::net::UdpSocket;

use str0m::{Event, Input, Output, change::SdpOffer};

use tracing::warn;

use crate::{
    metrics,
    sfu::error::{ClientError, ClientResult},
    types::{Peer, PollResult, S2cDcPayload},
};

impl Peer {
    pub fn poll(&mut self, socket: &UdpSocket) -> ClientResult<PollResult> {
        loop {
            match self.rtc.poll_output()? {
                Output::Timeout(v) => return Ok(PollResult::Timeout(v)),
                Output::Transmit(t) => {
                    socket.send_to(&t.contents, t.destination)?;
                    metrics::sent(t.contents.len());
                }
                Output::Event(Event::ChannelOpen(cid, _label)) => self.cid = Some(cid),
                Output::Event(e) => return Ok(PollResult::Event(e)),
            }
        }
    }

    pub fn handle_offer(&mut self, offer: &str) -> ClientResult<()> {
        let offer = SdpOffer::from_sdp_string(offer)?;

        let answer = self.rtc.sdp_api().accept_offer(offer)?;

        if let Some(mut channel) = self.cid.and_then(|id| self.rtc.channel(id)) {
            let json = serde_json::to_string(&answer)?;
            channel.write(false, json.as_bytes())?;
        };

        Ok(())
    }

    pub fn handle_input(&mut self, input: Input) {
        if !self.rtc.is_alive() {
            return;
        }
        if let Err(e) = self.rtc.handle_input(input) {
            warn!("Client ({:?}) disconnected: {:?}", self, e);
            self.rtc.disconnect();
        }
    }

    pub fn send_payload_via_dc(&mut self, payload: S2cDcPayload) -> ClientResult<()> {
        let mut channel = self
            .cid
            .and_then(|id| self.rtc.channel(id))
            .ok_or(ClientError::ChannelNotFound)?;

        let json = serde_json::to_string(&payload)?;

        channel.write(false, json.as_bytes())?;

        Ok(())
    }
}
