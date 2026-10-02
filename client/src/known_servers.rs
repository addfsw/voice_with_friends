//! Запомненные отпечатки сертификатов серверов (как `known_hosts` в SSH).
//!
//! Файл текстовый, по строке на сервер: `адрес отпечаток`. Его можно
//! открыть блокнотом и удалить строку, если сервер честно переустановили.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

#[derive(Debug, Default)]
pub struct KnownServers {
    /// `None` — только в памяти (для тестов).
    path: Option<PathBuf>,
    entries: BTreeMap<String, String>,
}

impl KnownServers {
    /// Загружает файл; если его ещё нет — пустой список.
    pub fn load(path: impl Into<PathBuf>) -> Self {
        let path = path.into();
        let entries = std::fs::read_to_string(&path)
            .map(|text| {
                text.lines()
                    .map(str::trim)
                    .filter(|line| !line.is_empty() && !line.starts_with('#'))
                    .filter_map(|line| line.split_once(char::is_whitespace))
                    .map(|(addr, fp)| (addr.to_string(), fp.trim().to_string()))
                    .collect()
            })
            .unwrap_or_default();
        Self { path: Some(path), entries }
    }

    pub fn in_memory() -> Self {
        Self::default()
    }

    /// Запомненный отпечаток сервера по адресу (`192.168.1.10:47777`).
    pub fn get(&self, address: &str) -> Option<&str> {
        self.entries.get(address).map(String::as_str)
    }

    /// Запоминает (или заменяет) отпечаток и сразу сохраняет файл.
    pub fn remember(&mut self, address: &str, fingerprint: &str) -> std::io::Result<()> {
        self.entries.insert(address.to_string(), fingerprint.to_string());
        self.save()
    }

    fn save(&self) -> std::io::Result<()> {
        let Some(path) = &self.path else { return Ok(()) };
        if let Some(dir) = path.parent() {
            std::fs::create_dir_all(dir)?;
        }
        let mut text = String::from(
            "# Поигрулькины: запомненные сертификаты серверов.\n\
             # Строка: адрес отпечаток. Удалите строку, если сервер переустановили.\n",
        );
        for (address, fingerprint) in &self.entries {
            text.push_str(&format!("{address} {fingerprint}\n"));
        }
        std::fs::write(path, text)
    }
}

/// Где консольный клиент хранит файл: `%APPDATA%\poigrulkiny\known_servers.txt`
/// на Windows, `~/.config/poigrulkiny/known_servers.txt` на Linux.
pub fn default_path() -> PathBuf {
    let base = std::env::var_os("APPDATA")
        .or_else(|| std::env::var_os("XDG_CONFIG_HOME"))
        .map(PathBuf::from)
        .or_else(|| std::env::var_os("HOME").map(|home| Path::new(&home).join(".config")))
        .unwrap_or_else(|| PathBuf::from("."));
    base.join("poigrulkiny").join("known_servers.txt")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn save_and_load() {
        let dir = std::env::temp_dir().join(format!("vwf-known-{}", std::process::id()));
        let path = dir.join("known.txt");
        let mut known = KnownServers::load(&path);
        assert_eq!(known.get("1.2.3.4:47777"), None);
        known.remember("1.2.3.4:47777", "AA:BB").unwrap();
        known.remember("5.6.7.8:47777", "CC:DD").unwrap();
        known.remember("1.2.3.4:47777", "EE:FF").unwrap();

        let loaded = KnownServers::load(&path);
        assert_eq!(loaded.get("1.2.3.4:47777"), Some("EE:FF"));
        assert_eq!(loaded.get("5.6.7.8:47777"), Some("CC:DD"));
        std::fs::remove_dir_all(dir).unwrap();
    }
}
