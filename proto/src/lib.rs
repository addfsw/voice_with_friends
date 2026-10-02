//! Протокол общения клиента и сервера.
//!
//! Всё идёт поверх одного QUIC-соединения:
//! - **управляющий канал** — один двунаправленный QUIC-поток, который открывает клиент.
//!   В нём ходят сообщения [`ClientMsg`] / [`ServerMsg`], каждое закодировано postcard'ом
//!   и предварено длиной (u32 LE). Этот канал надёжный и упорядоченный.
//! - **голос** — QUIC-датаграммы (ненадёжные, как обычный UDP, но зашифрованные).
//!   Потерянный голосовой пакет переотправлять бессмысленно: пока он дойдёт,
//!   его время проигрывания уже пройдёт.

use serde::{Deserialize, Serialize};
use tokio::io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt};

/// Версия протокола. Сервер отказывает клиентам с другой версией.
/// 2 — `seq` голосовых пакетов растёт и в тишине (см. ниже).
pub const PROTOCOL_VERSION: u32 = 2;

/// ALPN-идентификатор для TLS-рукопожатия QUIC.
pub const ALPN: &[u8] = b"vwf/1";

/// Порт сервера по умолчанию (UDP).
pub const DEFAULT_PORT: u16 = 47777;

/// Частота дискретизации всего «сетевого» звука. Opus внутри работает на 48 кГц,
/// поэтому всё, что приходит со звуковой карты, приводится к этой частоте.
pub const SAMPLE_RATE: u32 = 48_000;

/// Длительность одного голосового кадра. 20 мс — стандарт для VoIP:
/// компромисс между задержкой и накладными расходами на заголовки пакетов.
pub const FRAME_MS: u32 = 20;

/// Количество сэмплов (моно) в одном кадре: 48000 * 0.020 = 960.
pub const FRAME_SAMPLES: usize = (SAMPLE_RATE * FRAME_MS / 1000) as usize;

/// Максимальный размер управляющего сообщения — защита от мусора в канале.
const MAX_CONTROL_MSG: usize = 64 * 1024;

/// Идентификатор участника, который выдаёт сервер.
pub type PeerId = u32;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PeerInfo {
    pub id: PeerId,
    pub name: String,
}

// ---------------------------------------------------------------------------
// Поиск серверов в локальной сети.
//
// Клиент шлёт широковещательный UDP-пакет `DISCOVERY_PROBE` на порт
// `DISCOVERY_PORT`, и все серверы в сети отвечают ему лично пакетом
// `[DISCOVERY_REPLY_MAGIC][postcard(Announcement)]`. Широковещательные пакеты
// не выходят за пределы локальной сети (и не проходят через Tailscale),
// поэтому через интернет адрес по-прежнему вводится вручную.
// ---------------------------------------------------------------------------

/// UDP-порт, на котором серверы отвечают на поиск.
pub const DISCOVERY_PORT: u16 = 47778;

/// Запрос «есть тут серверы?».
pub const DISCOVERY_PROBE: &[u8] = b"VWF-DISCOVER-1";

const DISCOVERY_REPLY_MAGIC: &[u8] = b"VWF-HERE-1";

/// Ответ сервера на поиск.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Announcement {
    /// Случайный номер запуска сервера: по нему клиент понимает, что ответы,
    /// пришедшие через разные сетевые интерфейсы, — от одного и того же сервера.
    pub instance: u64,
    /// Название сервера (по умолчанию — имя компьютера).
    pub name: String,
    /// UDP-порт QUIC, к которому подключаться.
    pub port: u16,
    pub protocol: u32,
    /// Сколько человек сейчас в комнате.
    pub users: u32,
}

pub fn encode_announcement(announcement: &Announcement) -> Vec<u8> {
    let mut buf = DISCOVERY_REPLY_MAGIC.to_vec();
    buf.extend(postcard::to_allocvec(announcement).expect("сериализация в память не падает"));
    buf
}

pub fn decode_announcement(data: &[u8]) -> Option<Announcement> {
    postcard::from_bytes(data.strip_prefix(DISCOVERY_REPLY_MAGIC)?).ok()
}

/// Сообщения клиент → сервер.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum ClientMsg {
    /// Первое сообщение после подключения.
    Hello { protocol: u32, name: String },
}

/// Сообщения сервер → клиент.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum ServerMsg {
    /// Ответ на `Hello`: id клиента и список тех, кто уже в комнате.
    Welcome {
        your_id: PeerId,
        peers: Vec<PeerInfo>,
    },
    PeerJoined(PeerInfo),
    PeerLeft {
        id: PeerId,
    },
    /// Сервер отказал в подключении (после этого соединение закрывается).
    Rejected {
        reason: String,
    },
}

/// Записывает сообщение в поток: `[длина u32 LE][postcard]`.
pub async fn write_msg<W, M>(w: &mut W, msg: &M) -> anyhow::Result<()>
where
    W: AsyncWrite + Unpin,
    M: Serialize,
{
    let body = postcard::to_allocvec(msg)?;
    w.write_all(&(body.len() as u32).to_le_bytes()).await?;
    w.write_all(&body).await?;
    Ok(())
}

/// Читает одно сообщение из потока. `Ok(None)` — поток аккуратно закрыт.
pub async fn read_msg<R, M>(r: &mut R) -> anyhow::Result<Option<M>>
where
    R: AsyncRead + Unpin,
    M: for<'de> Deserialize<'de>,
{
    let mut len = [0u8; 4];
    match r.read_exact(&mut len).await {
        Ok(_) => {}
        Err(e) if e.kind() == std::io::ErrorKind::UnexpectedEof => return Ok(None),
        Err(e) => return Err(e.into()),
    }
    let len = u32::from_le_bytes(len) as usize;
    anyhow::ensure!(len <= MAX_CONTROL_MSG, "управляющее сообщение слишком большое: {len} байт");
    let mut body = vec![0u8; len];
    r.read_exact(&mut body).await?;
    Ok(Some(postcard::from_bytes(&body)?))
}

// ---------------------------------------------------------------------------
// Голосовые датаграммы. Формат простой и бинарный, без сериализатора:
// клиент → сервер: [seq u32 LE][opus...]
// сервер → клиент: [отправитель u32 LE][seq u32 LE][opus...]
// `seq` — номер 20-мс кадра у отправителя. Он растёт на 1 каждые 20 мс, даже
// когда отправитель молчит и пакеты не шлёт (с версии протокола 2). По нему
// получатель восстанавливает порядок, замечает потери и измеряет джиттер.
// ---------------------------------------------------------------------------

/// Голосовой пакет от клиента к серверу.
pub fn encode_client_voice(seq: u32, opus: &[u8]) -> Vec<u8> {
    let mut buf = Vec::with_capacity(4 + opus.len());
    buf.extend_from_slice(&seq.to_le_bytes());
    buf.extend_from_slice(opus);
    buf
}

/// Разбирает пакет клиента: `(seq, opus)`.
pub fn decode_client_voice(data: &[u8]) -> Option<(u32, &[u8])> {
    let (seq, opus) = data.split_first_chunk::<4>()?;
    Some((u32::from_le_bytes(*seq), opus))
}

/// Пересылаемый сервером пакет: к пакету клиента добавляется id отправителя.
pub fn encode_server_voice(from: PeerId, client_packet: &[u8]) -> Vec<u8> {
    let mut buf = Vec::with_capacity(4 + client_packet.len());
    buf.extend_from_slice(&from.to_le_bytes());
    buf.extend_from_slice(client_packet);
    buf
}

/// Разбирает пакет сервера: `(отправитель, seq, opus)`.
pub fn decode_server_voice(data: &[u8]) -> Option<(PeerId, u32, &[u8])> {
    let (from, rest) = data.split_first_chunk::<4>()?;
    let (seq, opus) = decode_client_voice(rest)?;
    Some((PeerId::from_le_bytes(*from), seq, opus))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn voice_roundtrip() {
        let client = encode_client_voice(42, b"opus");
        assert_eq!(decode_client_voice(&client), Some((42, &b"opus"[..])));
        let server = encode_server_voice(7, &client);
        assert_eq!(decode_server_voice(&server), Some((7, 42, &b"opus"[..])));
        assert_eq!(decode_server_voice(&[1, 2, 3]), None);
    }

    #[test]
    fn announcement_roundtrip() {
        let a = Announcement {
            instance: 42,
            name: "Комната".into(),
            port: DEFAULT_PORT,
            protocol: PROTOCOL_VERSION,
            users: 3,
        };
        assert_eq!(decode_announcement(&encode_announcement(&a)), Some(a));
        assert_eq!(decode_announcement(DISCOVERY_PROBE), None);
        assert_eq!(decode_announcement(b"VWF-HERE-1garbage"), None);
    }

    #[tokio::test]
    async fn control_roundtrip() {
        let msg = ServerMsg::Welcome {
            your_id: 3,
            peers: vec![PeerInfo { id: 1, name: "Алиса".into() }],
        };
        let mut buf = Vec::new();
        write_msg(&mut buf, &msg).await.unwrap();
        let mut rd = &buf[..];
        let back: Option<ServerMsg> = read_msg(&mut rd).await.unwrap();
        assert_eq!(back, Some(msg));
        let eof: Option<ServerMsg> = read_msg(&mut rd).await.unwrap();
        assert_eq!(eof, None);
    }
}
