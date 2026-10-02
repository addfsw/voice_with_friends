//! Код приглашения: `poi-` + base64 от темы почтового ящика и отпечатка
//! сертификата сервера. Код постоянный: отправили друзьям один раз — и он
//! работает, пока хост не сменит компьютер или сертификат.
//!
//! Отпечаток в коде даёт защиту от подмены уже с первого подключения:
//! гость точно знает, какой сертификат должен предъявить сервер.

use base64::Engine;
use base64::engine::general_purpose::URL_SAFE_NO_PAD;

pub const PREFIX: &str = "poi-";
const TOPIC_BYTES: usize = 16;
const FINGERPRINT_BYTES: usize = 32;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Invite {
    /// Случайный идентификатор комнаты (из него получается тема ящика).
    pub room: [u8; TOPIC_BYTES],
    /// Отпечаток сертификата в виде `4E:ED:...`.
    pub fingerprint: String,
}

impl Invite {
    pub fn new(room: [u8; TOPIC_BYTES], fingerprint: &str) -> Self {
        Self { room, fingerprint: fingerprint.to_string() }
    }

    /// Тема в почтовом ящике.
    pub fn topic(&self) -> String {
        format!("poigrulkiny-{}", hex(&self.room))
    }

    pub fn encode(&self) -> String {
        let mut bytes = self.room.to_vec();
        bytes.extend(fingerprint_bytes(&self.fingerprint).unwrap_or([0; FINGERPRINT_BYTES]));
        format!("{PREFIX}{}", URL_SAFE_NO_PAD.encode(bytes))
    }

    pub fn decode(code: &str) -> anyhow::Result<Self> {
        let body = code
            .trim()
            .strip_prefix(PREFIX)
            .ok_or_else(|| anyhow::anyhow!("это не код приглашения"))?;
        let bytes = URL_SAFE_NO_PAD
            .decode(body)
            .map_err(|_| anyhow::anyhow!("код приглашения повреждён — скопируйте его целиком"))?;
        if bytes.len() != TOPIC_BYTES + FINGERPRINT_BYTES {
            anyhow::bail!("код приглашения повреждён — скопируйте его целиком");
        }
        let room = bytes[..TOPIC_BYTES].try_into().expect("длина проверена");
        let fingerprint =
            bytes[TOPIC_BYTES..].iter().map(|b| format!("{b:02X}")).collect::<Vec<_>>().join(":");
        Ok(Self { room, fingerprint })
    }
}

/// Идентификатор комнаты хранится в файле, чтобы код приглашения не менялся
/// между запусками.
pub fn load_or_create_room(path: &std::path::Path) -> std::io::Result<[u8; TOPIC_BYTES]> {
    if let Ok(text) = std::fs::read_to_string(path) {
        let bytes: Option<Vec<u8>> = (0..text.trim().len())
            .step_by(2)
            .map(|i| text.trim().get(i..i + 2).and_then(|h| u8::from_str_radix(h, 16).ok()))
            .collect();
        if let Some(Ok(room)) = bytes.map(<[u8; TOPIC_BYTES]>::try_from) {
            return Ok(room);
        }
    }
    let room = crate::new_room_id();
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir)?;
    }
    std::fs::write(path, hex(&room))?;
    Ok(room)
}

pub fn looks_like_invite(text: &str) -> bool {
    text.trim().starts_with(PREFIX)
}

fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

fn fingerprint_bytes(fingerprint: &str) -> Option<[u8; FINGERPRINT_BYTES]> {
    let bytes: Vec<u8> = fingerprint
        .split(':')
        .map(|part| u8::from_str_radix(part, 16))
        .collect::<Result<_, _>>()
        .ok()?;
    bytes.try_into().ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn roundtrip() {
        let fingerprint = (0..32).map(|i| format!("{:02X}", i * 7)).collect::<Vec<_>>().join(":");
        let invite = Invite::new([9; 16], &fingerprint);
        let code = invite.encode();
        assert!(looks_like_invite(&code));
        assert_eq!(code.len(), PREFIX.len() + 64);
        assert_eq!(Invite::decode(&format!("  {code}\n")).unwrap(), invite);
        assert!(Invite::decode("poi-abc").is_err());
        assert!(Invite::decode("192.168.1.10").is_err());
        assert!(invite.topic().starts_with("poigrulkiny-"));
    }
}
