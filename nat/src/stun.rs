//! Минимальный STUN (RFC 8489): только запрос «с какого адреса ты меня видишь?»
//! (Binding Request) и разбор ответа с XOR-MAPPED-ADDRESS.
//!
//! Публичные STUN-серверы (Google, Cloudflare) бесплатны и без регистрации:
//! они просто отвечают, с какого IP и порта к ним пришёл пакет. Это и есть
//! наш внешний адрес за NAT.

use std::net::{IpAddr, Ipv4Addr, SocketAddr};

pub const MAGIC_COOKIE: u32 = 0x2112_A442;
const BINDING_REQUEST: u16 = 0x0001;
const BINDING_RESPONSE: u16 = 0x0101;
const ATTR_MAPPED_ADDRESS: u16 = 0x0001;
const ATTR_XOR_MAPPED_ADDRESS: u16 = 0x0020;
const HEADER_LEN: usize = 20;

pub type TransactionId = [u8; 12];

/// Серверы по умолчанию. Переопределяются переменной окружения
/// `VWF_STUN_SERVERS` (через запятую) — так сделан испытательный стенд.
pub const DEFAULT_SERVERS: &[&str] =
    &["stun.l.google.com:19302", "stun.cloudflare.com:3478", "stun1.l.google.com:19302"];

pub fn servers() -> Vec<String> {
    match std::env::var("VWF_STUN_SERVERS") {
        Ok(list) if !list.trim().is_empty() => {
            list.split(',').map(|s| s.trim().to_string()).filter(|s| !s.is_empty()).collect()
        }
        _ => DEFAULT_SERVERS.iter().map(|s| s.to_string()).collect(),
    }
}

pub fn binding_request(id: &TransactionId) -> [u8; HEADER_LEN] {
    let mut msg = [0u8; HEADER_LEN];
    msg[0..2].copy_from_slice(&BINDING_REQUEST.to_be_bytes());
    // Длина атрибутов — 0.
    msg[4..8].copy_from_slice(&MAGIC_COOKIE.to_be_bytes());
    msg[8..20].copy_from_slice(id);
    msg
}

/// Похоже ли это на STUN-сообщение. У пакетов QUIC старшие два бита первого
/// байта почти всегда не нули, а «волшебное число» на месте 4..8 отсекает остальное.
pub fn is_stun(data: &[u8]) -> bool {
    data.len() >= HEADER_LEN && data[0] & 0xC0 == 0 && data[4..8] == MAGIC_COOKIE.to_be_bytes()
}

/// Разбирает ответ на Binding Request: номер транзакции и наш внешний адрес.
pub fn parse_binding_response(data: &[u8]) -> Option<(TransactionId, SocketAddr)> {
    if !is_stun(data) || u16::from_be_bytes([data[0], data[1]]) != BINDING_RESPONSE {
        return None;
    }
    let len = u16::from_be_bytes([data[2], data[3]]) as usize;
    let attrs = data.get(HEADER_LEN..HEADER_LEN + len)?;
    let id: TransactionId = data[8..20].try_into().ok()?;

    let mut mapped = None;
    let mut rest = attrs;
    while rest.len() >= 4 {
        let kind = u16::from_be_bytes([rest[0], rest[1]]);
        let value_len = u16::from_be_bytes([rest[2], rest[3]]) as usize;
        let value = rest.get(4..4 + value_len)?;
        match kind {
            ATTR_XOR_MAPPED_ADDRESS => return Some((id, decode_address(value, true)?)),
            ATTR_MAPPED_ADDRESS => mapped = decode_address(value, false),
            _ => {}
        }
        // Атрибуты выровнены по 4 байта.
        let padded = (4 + value_len).div_ceil(4) * 4;
        rest = rest.get(padded..).unwrap_or(&[]);
    }
    mapped.map(|addr| (id, addr))
}

/// Только IPv4: программа пока работает только по нему.
fn decode_address(value: &[u8], xor: bool) -> Option<SocketAddr> {
    if value.len() < 8 || value[1] != 0x01 {
        return None;
    }
    let mut port = u16::from_be_bytes([value[2], value[3]]);
    let mut ip = u32::from_be_bytes([value[4], value[5], value[6], value[7]]);
    if xor {
        port ^= (MAGIC_COOKIE >> 16) as u16;
        ip ^= MAGIC_COOKIE;
    }
    Some(SocketAddr::new(IpAddr::V4(Ipv4Addr::from(ip)), port))
}

/// Ответ STUN-сервера. Нужен для тестов и испытательного стенда.
pub fn binding_response(id: &TransactionId, addr: SocketAddr) -> Vec<u8> {
    let IpAddr::V4(ip) = addr.ip() else { panic!("только IPv4") };
    let mut msg = Vec::with_capacity(HEADER_LEN + 12);
    msg.extend_from_slice(&BINDING_RESPONSE.to_be_bytes());
    msg.extend_from_slice(&12u16.to_be_bytes());
    msg.extend_from_slice(&MAGIC_COOKIE.to_be_bytes());
    msg.extend_from_slice(id);
    msg.extend_from_slice(&ATTR_XOR_MAPPED_ADDRESS.to_be_bytes());
    msg.extend_from_slice(&8u16.to_be_bytes());
    msg.extend_from_slice(&[0, 0x01]);
    msg.extend_from_slice(&(addr.port() ^ (MAGIC_COOKIE >> 16) as u16).to_be_bytes());
    msg.extend_from_slice(&(u32::from(ip) ^ MAGIC_COOKIE).to_be_bytes());
    msg
}

/// Номер транзакции из запроса (для STUN-сервера на стенде).
pub fn request_id(data: &[u8]) -> Option<TransactionId> {
    if !is_stun(data) || u16::from_be_bytes([data[0], data[1]]) != BINDING_REQUEST {
        return None;
    }
    data[8..20].try_into().ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn request_response_roundtrip() {
        let id = [7u8; 12];
        let request = binding_request(&id);
        assert!(is_stun(&request));
        assert_eq!(request_id(&request), Some(id));

        let addr: SocketAddr = "203.0.113.7:51234".parse().unwrap();
        let response = binding_response(&id, addr);
        assert_eq!(parse_binding_response(&response), Some((id, addr)));
        assert_eq!(parse_binding_response(&request), None);
    }

    /// Реальные серверы добавляют в ответ свои атрибуты (например, SOFTWARE
    /// с названием программы). Их надо уметь пропускать с учётом выравнивания.
    #[test]
    fn parses_real_world_response_with_extra_attributes() {
        let id = [1u8; 12];
        let mut msg = binding_response(&id, "198.51.100.20:40000".parse().unwrap());
        // Добавим посторонний атрибут SOFTWARE (0x8022) длиной 5 — с выравниванием.
        msg.extend_from_slice(&[0x80, 0x22, 0x00, 0x05, b'g', b'o', b'o', b'g', b'l', 0, 0, 0]);
        let len = (msg.len() - HEADER_LEN) as u16;
        msg[2..4].copy_from_slice(&len.to_be_bytes());
        assert_eq!(
            parse_binding_response(&msg).map(|(_, a)| a),
            Some("198.51.100.20:40000".parse().unwrap())
        );
    }

    #[test]
    fn quic_packets_are_not_stun() {
        let mut long_header = [0u8; 40];
        long_header[0] = 0xC3;
        assert!(!is_stun(&long_header));
        let mut short_header = [0u8; 40];
        short_header[0] = 0x41;
        assert!(!is_stun(&short_header));
    }
}
