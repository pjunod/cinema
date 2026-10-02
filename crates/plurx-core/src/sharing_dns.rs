//! Tailscale Quad100 only. No system resolver or recursive public DNS fallback.
use crate::{
    error::StoreError,
    sharing::{is_tailnet_address, validate_tailnet_name},
    sharing_tls::{bound_tcp_socket, PinnedEgress},
};
use hickory_proto::{
    op::{Message, MessageType, OpCode, Query, ResponseCode},
    rr::{DNSClass, Name, RData, RecordType},
};
use std::{
    net::{IpAddr, Ipv4Addr, Ipv6Addr, SocketAddr},
    time::Duration,
};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
fn unavailable() -> StoreError {
    StoreError::Identity("Tailscale resolver unavailable".into())
}
const MAX_REPLY: usize = 4096;
fn response(bytes: &[u8], query: &Message) -> Result<Vec<IpAddr>, StoreError> {
    if bytes.len() < 12 || bytes.len() > MAX_REPLY {
        return Err(unavailable());
    }
    let count = |offset| u16::from_be_bytes([bytes[offset], bytes[offset + 1]]);
    // Reject forged huge counts before the DNS library allocates record lists.
    if count(4) != 1 || count(6) > 4 || count(8) > 8 || count(10) > 8 {
        return Err(unavailable());
    }
    let message = Message::from_vec(bytes).map_err(|_| unavailable())?;
    if message.metadata.id != query.metadata.id
        || message.metadata.message_type != MessageType::Response
        || message.metadata.op_code != OpCode::Query
        || message.metadata.response_code != ResponseCode::NoError
        || message.metadata.truncation
        || message.queries != query.queries
    {
        return Err(unavailable());
    }
    let question = &query.queries[0];
    let mut addresses = Vec::new();
    for answer in &message.answers {
        if &answer.name != question.name()
            || answer.dns_class != DNSClass::IN
            || answer.record_type() != question.query_type()
        {
            return Err(unavailable());
        }
        let address = match &answer.data {
            RData::A(ip) => IpAddr::V4(ip.0),
            RData::AAAA(ip) => IpAddr::V6(ip.0),
            _ => return Err(unavailable()),
        };
        if !is_tailnet_address(address) {
            return Err(unavailable());
        }
        if !addresses.contains(&address) {
            addresses.push(address);
        }
    }
    // A MagicDNS machine has one address in each family, not a public pool.
    if addresses.len() > 1 {
        return Err(unavailable());
    }
    Ok(addresses)
}
fn resolver(egress: &PinnedEgress) -> SocketAddr {
    let ip = if matches!(egress, PinnedEgress::LocalAddress(IpAddr::V6(_))) {
        IpAddr::V6(Ipv6Addr::new(0xfd7a, 0x115c, 0xa1e0, 0, 0, 0, 0, 0x53))
    } else {
        IpAddr::V4(Ipv4Addr::new(100, 100, 100, 100))
    };
    SocketAddr::new(ip, 53)
}
async fn question(
    name: &Name,
    kind: RecordType,
    egress: &PinnedEgress,
) -> Result<Vec<IpAddr>, StoreError> {
    let resolver = resolver(egress);
    let bind = match egress {
        PinnedEgress::LocalAddress(ip) if !ip.is_unspecified() && !ip.is_multicast() => {
            SocketAddr::new(*ip, 0)
        }
        PinnedEgress::LocalAddress(_) => return Err(unavailable()),
        PinnedEgress::Interface(_) => SocketAddr::from(([0, 0, 0, 0], 0)),
    };
    let socket = tokio::net::UdpSocket::bind(bind)
        .await
        .map_err(|_| unavailable())?;
    if let PinnedEgress::Interface(interface) = egress {
        if interface.is_empty() || interface.len() > 15 || interface.as_bytes().contains(&0) {
            return Err(unavailable());
        }
        #[cfg(target_os = "linux")]
        socket
            .bind_device(Some(interface.as_bytes()))
            .map_err(|_| unavailable())?;
        #[cfg(not(target_os = "linux"))]
        return Err(unavailable());
    }
    socket.connect(resolver).await.map_err(|_| unavailable())?;
    let mut query = Message::query();
    query.add_query(Query::query(name.clone(), kind));
    let bytes = query.to_vec().map_err(|_| unavailable())?;
    socket.send(&bytes).await.map_err(|_| unavailable())?;
    let mut reply = [0u8; MAX_REPLY + 1];
    let read = socket.recv(&mut reply).await.map_err(|_| unavailable())?;
    if read > MAX_REPLY {
        return Err(unavailable());
    }
    if read >= 12 && reply[2] & 2 != 0 {
        // DNS truncation retries only the same fixed resolver over TCP.
        let mut tcp = bound_tcp_socket(resolver, egress)?
            .connect(resolver)
            .await
            .map_err(|_| unavailable())?;
        tcp.write_u16(bytes.len() as u16)
            .await
            .map_err(|_| unavailable())?;
        tcp.write_all(&bytes).await.map_err(|_| unavailable())?;
        let length = tcp.read_u16().await.map_err(|_| unavailable())? as usize;
        if !(12..=MAX_REPLY).contains(&length) {
            return Err(unavailable());
        }
        let mut reply = vec![0u8; length];
        tcp.read_exact(&mut reply)
            .await
            .map_err(|_| unavailable())?;
        response(&reply, &query)
    } else {
        response(&reply[..read], &query)
    }
}
/// Resolve a full validated tailnet name through the node-local resolver,
/// bind to the qualified egress, and return numeric answers for a pinned dial.
pub async fn resolve_tailnet(name: &str, egress: &PinnedEgress) -> Result<Vec<IpAddr>, StoreError> {
    validate_tailnet_name(name)?;
    let name = Name::from_ascii(format!("{name}.")).map_err(|_| unavailable())?;
    tokio::time::timeout(Duration::from_secs(1), async {
        let (v4, v6) = tokio::join!(
            question(&name, RecordType::A, egress),
            question(&name, RecordType::AAAA, egress)
        );
        match (v4, v6) {
            (Ok(mut v4), Ok(v6)) => {
                v4.extend(v6);
                Ok(v4)
            }
            (Ok(addresses), Err(_)) | (Err(_), Ok(addresses)) if !addresses.is_empty() => {
                Ok(addresses)
            }
            _ => Err(unavailable()),
        }
    })
    .await
    .map_err(|_| unavailable())?
}
#[cfg(test)]
mod tests {
    use super::*;
    use hickory_proto::rr::{
        rdata::{A, AAAA},
        Record,
    };
    fn fixture(kind: RecordType, data: RData) -> (Message, Message) {
        let mut query = Message::query();
        let name = Name::from_ascii("source.fixture.ts.net.").expect("fixture name");
        query.add_query(Query::query(name.clone(), kind));
        let mut reply = Message::response(query.metadata.id, OpCode::Query);
        reply.add_query(query.queries[0].clone());
        reply.add_answer(Record::from_rdata(name, 60, data));
        (query, reply)
    }
    #[test]
    fn sharing_dns_rejects_wrong_transaction_question_public_answers_and_unbounded_counts() {
        let (query, mut reply) = fixture(
            RecordType::A,
            RData::A(A(Ipv4Addr::new(100, 101, 102, 103))),
        );
        assert_eq!(
            response(&reply.to_vec().expect("reply"), &query).expect("tailnet answer"),
            vec![IpAddr::V4(Ipv4Addr::new(100, 101, 102, 103))]
        );
        reply.metadata.id ^= 1;
        assert!(response(&reply.to_vec().expect("reply"), &query).is_err());
        reply.metadata.id ^= 1;
        reply.queries[0] = Query::query(
            Name::from_ascii("other.fixture.ts.net.").expect("name"),
            RecordType::A,
        );
        assert!(response(&reply.to_vec().expect("reply"), &query).is_err());
        for ip in [
            Ipv4Addr::new(192, 168, 4, 7),
            Ipv4Addr::new(127, 0, 0, 1),
            Ipv4Addr::new(8, 8, 8, 8),
            Ipv4Addr::new(100, 63, 255, 255),
            Ipv4Addr::new(100, 128, 0, 0),
        ] {
            let (query, reply) = fixture(RecordType::A, RData::A(A(ip)));
            assert!(response(&reply.to_vec().expect("reply"), &query).is_err());
        }
        let (query, reply) = fixture(
            RecordType::AAAA,
            RData::AAAA(AAAA("fd7a:115c:a1e0::1".parse().expect("tailnet v6"))),
        );
        assert!(response(&reply.to_vec().expect("reply"), &query).is_ok());
        let (query, reply) = fixture(
            RecordType::AAAA,
            RData::AAAA(AAAA("::ffff:100.101.102.103".parse().expect("mapped v6"))),
        );
        assert!(response(&reply.to_vec().expect("reply"), &query).is_err());
        let mut bytes = reply.to_vec().expect("reply");
        bytes[6] = 255;
        bytes[7] = 255;
        assert!(response(&bytes, &query).is_err());
        assert!(response(&vec![0u8; MAX_REPLY + 1], &query).is_err());
    }
    #[tokio::test]
    async fn sharing_dns_rejects_non_tailnet_names_before_network_io() {
        for name in [
            "example.org",
            "source.ts.net",
            "Source.fixture.ts.net",
            "http://source.fixture.ts.net",
        ] {
            assert!(
                resolve_tailnet(name, &PinnedEgress::Interface("unavailable".into()))
                    .await
                    .is_err()
            );
        }
    }
}
