use loong_reverse_formats::pcap::{Decoded, decode};
use loong_reverse_testkit::pcap::{dns_response, ipv4_pkt, udp};

fn decoded(datagram: &[u8]) -> Decoded {
    decode(101, &ipv4_pkt(17, [192, 0, 2, 1], [192, 0, 2, 2], datagram))
}

#[test]
fn bytes_past_the_udp_length_are_not_dns() {
    let mut datagram = udp(53, 50000, &[]);
    datagram.extend(dns_response());
    let result = decoded(&datagram);
    assert_eq!(result.transport.unwrap().payload_len, 0);
    assert!(result.dns.is_none());
}

#[test]
fn impossible_udp_lengths_do_not_expose_payload() {
    for length in 0..8u16 {
        let mut datagram = udp(53, 50000, &dns_response());
        datagram[4..6].copy_from_slice(&length.to_be_bytes());
        let result = decoded(&datagram);
        let transport = result.transport.unwrap();
        assert_eq!(
            (transport.src_port, transport.dst_port),
            (Some(53), Some(50000))
        );
        assert_eq!(transport.payload_len, 0, "length {length}");
        assert!(result.dns.is_none());
        assert!(result.note.is_some());
    }
}

#[test]
fn shortened_udp_headers_and_bodies_are_reported() {
    let whole = udp(53, 50000, &dns_response());
    for end in [4, 5, 6, 7, whole.len() - 1] {
        let result = decoded(&whole[..end]);
        assert!(result.note.is_some(), "cut at {end}");
        assert_eq!(result.transport.unwrap().src_port, Some(53));
    }
}

#[test]
fn a_complete_udp_dns_datagram_still_decodes() {
    let message = dns_response();
    let result = decoded(&udp(53, 50000, &message));
    assert_eq!(result.transport.unwrap().payload_len, message.len());
    assert_eq!(result.dns.unwrap().answers[0].value, "93.184.216.34");
    assert!(result.note.is_none());
}
