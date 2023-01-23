use std::{
    cmp::Ordering,
    io::{self, Write},
};

enum State {
    // Listen,
    SynRcvd,
    Estab,
    FinWait1,
    FinWait2,
    TimeWait,
}

impl State {
    fn is_synchoronized(&self) -> bool {
        match *self {
            State::SynRcvd => false,
            State::Estab | State::FinWait1 | State::FinWait2 | State::TimeWait => true,
        }
    }
}

pub struct Connection {
    state: State,
    send: SendSequenceSpace,
    recv: RecvSequenceSpace,
    ip: etherparse::Ipv4Header,
    tcp: etherparse::TcpHeader,
}

/// State of Send Sequence Space (RFC 793 S3.2)
///
/// ```
///                    1         2          3          4
///               ----------|----------|----------|----------
///                      SND.UNA    SND.NXT    SND.UNA
///                                           +SND.WND
///
/// 1 - old sequence numbers which have been acknowledged
/// 2 - sequence numbers of unacknowledged data
/// 3 - sequence numbers allowed for new data transmission
/// 4 - future sequence numbers which are not yet allowed
/// ```
///
///                           Send Sequence Space
///
///                                Figure 4.
struct SendSequenceSpace {
    /// send unacknowledged
    una: u32,
    /// send next
    nxt: u32,
    /// send window
    wnd: u16,
    /// send urgent pointer
    up: bool,
    wl1: usize,
    wl2: usize,
    /// initial receive sequence number
    iss: u32,
}

/// State of Receive Sequence Space (RFC 793 S3.2)
///
/// ```
///                        1          2          3
///                    ----------|----------|----------
///                           RCV.NXT    RCV.NXT
///                                     +RCV.WND
///
/// 1 - old sequence numbers which have been acknowledged
/// 2 - sequence numbers allowed for new reception
/// 3 - future sequence numbers which are not yet allowed
/// ```
///
///                          Receive Sequence Space
///
///                                Figure 5.
struct RecvSequenceSpace {
    /// receive next
    nxt: u32,
    /// receive window
    wnd: u16,
    /// receive urgent pointer
    up: bool,
    /// initial receive sequence number
    irs: u32,
}

impl Connection {
    pub fn accept<'a>(
        nic: &mut tun_tap::Iface,
        iph: etherparse::Ipv4HeaderSlice<'a>,
        tcph: etherparse::TcpHeaderSlice<'a>,
        data: &'a [u8],
    ) -> io::Result<Option<Self>> {
        let buf = [0u8; 1500];

        if !tcph.syn() {
            // only expected SYN packet
            return Ok(None);
        }

        let iss = 0;
        let wnd = 10;
        let mut c = Connection {
            state: State::SynRcvd,
            send: SendSequenceSpace {
                iss,
                una: iss,
                nxt: iss + 1,
                wnd,
                up: false,
                wl1: 0,
                wl2: 0,
            },
            recv: RecvSequenceSpace {
                nxt: tcph.sequence_number() + 1,
                wnd: tcph.window_size(),
                irs: tcph.sequence_number(),
                up: false,
            },
            ip: etherparse::Ipv4Header::new(
                0,
                64,
                etherparse::IpNumber::Tcp as u8,
                [
                    iph.destination()[0],
                    iph.destination()[1],
                    iph.destination()[2],
                    iph.destination()[3],
                ],
                [
                    iph.source()[0],
                    iph.source()[1],
                    iph.source()[2],
                    iph.source()[3],
                ],
            ),
            tcp: etherparse::TcpHeader::new(tcph.destination_port(), tcph.source_port(), iss, wnd),
        };

        // need to start establishing a connection
        c.tcp.syn = true;
        c.tcp.ack = true;
        c.write(nic, &[])?;

        Ok(Some(c))
    }

    fn write(&mut self, nic: &mut tun_tap::Iface, payload: &[u8]) -> io::Result<usize> {
        let mut buf = [0u8; 1500];
        self.tcp.sequence_number = self.send.nxt;
        self.tcp.acknowledgment_number = self.recv.nxt;

        let size = std::cmp::min(
            buf.len(),
            self.tcp.header_len() as usize + self.ip.header_len() + payload.len(),
        );
        self.ip.set_payload_len(size - self.ip.header_len());

        // the kernel is nice and does this for us
        self.tcp.checksum = self
            .tcp
            .calc_checksum_ipv4(&self.ip, &[])
            .expect("failed to compute checksum");

        // write out the header
        let mut unwritten = &mut buf[..];
        self.ip.write(&mut unwritten);
        self.tcp.write(&mut unwritten);
        let payload_bytes = unwritten.write(payload)?;
        let unwritten = unwritten.len();
        self.send.nxt = self.send.nxt.wrapping_add(payload_bytes as u32);
        if self.tcp.syn {
            self.send.nxt = self.send.nxt.wrapping_add(1);
            self.tcp.syn = false;
        }
        if self.tcp.fin {
            self.send.nxt = self.send.nxt.wrapping_add(1);
            self.tcp.fin = false;
        }
        nic.send(&buf[..buf.len() - unwritten])?;
        Ok(payload_bytes)
    }

    pub fn send_rst(&mut self, nic: &mut tun_tap::Iface) -> io::Result<()> {
        self.tcp.rst = true;
        // TODO: fix sequence numbers here
        self.tcp.sequence_number = 0;
        self.tcp.acknowledgment_number = 0;
        self.write(nic, &[])?;
        Ok(())
    }

    pub fn on_packet<'a>(
        &mut self,
        nic: &mut tun_tap::Iface,
        iph: etherparse::Ipv4HeaderSlice<'a>,
        tcph: etherparse::TcpHeaderSlice<'a>,
        data: &'a [u8],
    ) -> io::Result<()> {
        // first, check that sequence numbers are valid
        let seqn = tcph.sequence_number();
        let mut slen = data.len() as u32;
        if tcph.fin() {
            slen += 1;
        }
        if tcph.syn() {
            slen += 1;
        }
        let wend = self.recv.nxt.wrapping_add(self.recv.wnd as u32);
        if slen == 0 {
            // zero-length segment has separate rules for acceptance
            if self.recv.wnd == 0 {
                if seqn != self.recv.nxt {
                    return Ok(());
                }
            } else if !is_between_wrapped(self.recv.nxt.wrapping_sub(1), seqn, wend) {
                return Ok(());
            }
        } else {
            if self.recv.wnd == 0 {
                return Ok(());
            } else if !is_between_wrapped(self.recv.nxt.wrapping_sub(1), seqn, wend)
                && !is_between_wrapped(
                    self.recv.nxt.wrapping_sub(1),
                    seqn.wrapping_add(slen - 1),
                    wend,
                )
            {
                return Ok(());
            }
        }

        self.recv.nxt = seqn.wrapping_add(slen);
        // TODO: if _not_acceptable, send ACK
        // <SEQ=SND.NXT><ACK=RCV.NXT><CTL=ACK>
        //
        // valid segment check, ok if it acks at least one byte, which means that at least one of
        // the following is true:
        //
        // RCV.NXT =< SEG.SEQ < RCV.NXT + RCV.WND
        // RCV.NXT =< SEG.SEQ+SEG.LEN-1 < RCV.NXT+RCV.WND
        //

        // if tcph.acknowledgment_number()

        let ackn = tcph.acknowledgment_number();
        if let State::SynRcvd = self.state {
            // expect to get an ACK for out SYN
            if is_between_wrapped(
                self.send.una.wrapping_sub(1),
                ackn,
                self.send.nxt.wrapping_add(1),
            ) {
                // must have ACKed our SYN, since we detected at least one acked byte,
                // and we have only sent one byte (the SYN).
                self.state = State::Estab;
            } else {
                // TODO: RST: <SEQ=SEG.ACK><CTL=RST>
            }
        }

        if let State::Estab | State::FinWait1 | State::FinWait2 = self.state {
            if !is_between_wrapped(self.send.una, ackn, self.send.nxt.wrapping_add(1)) {
                return Ok(());
            }
            self.send.una = ackn;

            if let State::Estab = self.state {
                // now let's terminate the connection
                // TODO:
                assert!(data.is_empty());
                // TODO: needs to be stored in the retransmission queue!
                self.tcp.fin = true;
                self.write(nic, &[])?;
                self.state = State::FinWait1;
            }
        }

        if let State::FinWait1 = self.state {
            if self.send.una == self.send.iss + 2 {
                // our FIN has been ACKed!
                self.state = State::FinWait2;
            }
        }

        if tcph.fin() {
            match self.state {
                State::FinWait2 => {
                    // we're done with the connection!
                    self.tcp.fin = true;
                    self.write(nic, &[])?;
                    self.state = State::FinWait1;
                }
                _ => unreachable!(),
            }
        }

        // if let State::FinWait2 = self.state {
        //     if !tcph.fin() || !data.is_empty() {
        //         unimplemented!();
        //     }

        //     // must have ACKed our FIN, since we detected at least one acked byte,
        //     // and we have only sent one byte (the FIN).
        //     self.write(nic, &[])?;
        //     self.state = State::TimeWait;
        // }

        Ok(())
    }
}

fn is_between_wrapped(start: u32, x: u32, end: u32) -> bool {
    match start.cmp(&x) {
        Ordering::Equal => false,
        Ordering::Less => {
            // we have:
            //
            //     |------------S----------X--------------------|
            //
            // X is between S and E (S < X < E) in these cases:
            //
            //     |------------S----------X-----E--------------|
            //
            //     |--------E---S----------X--------------------|
            //
            // but *not* in these cases
            //
            //     |------------S---E------X--------------------|
            //
            //     |------------|----------X--------------------|
            //                  ^-S+E
            //
            //     |------------S----------|--------------------|
            //                             ^-X+E
            //
            //or, in other words, iff !(S <= E <= X)
            if end >= start && end <= x {
                return false;
            }
            return true;
        }
        Ordering::Greater => {
            // check is okay iff n is between u and a
            // we have the opposite above:
            //
            //     |------------X----------S--------------------|
            //
            // X is between S and E (S < X < E) only in this cases:
            //
            //     |------------X---E------S--------------------|
            //
            // but *not* in these cases
            //
            //     |------------X---S------E--------------------|
            //
            //     |---------E--X----------S--------------------|
            //
            //     |------------|----------S--------------------|
            //                  ^-X+E
            //
            //     |------------X----------|--------------------|
            //                             ^-S+E
            //
            //or, in other words, iff !(S < E < X) (iff => if and only if)
            if end < start && end > x {
            } else {
                return false;
            }
            return true;
        }
    }
}
