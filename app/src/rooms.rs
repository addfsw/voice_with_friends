//! Свои комнаты хоста: у каждой своё название и свой код приглашения.
//!
//! Код приглашения ведёт в конкретную комнату (через свой «почтовый ящик»,
//! см. `vwf_nat::invite`). Пока открыта комната «Игры с Саней», код комнаты
//! «Работа» ни к чему не приведёт: хоста по нему просто нет. Так разные
//! компании друзей не попадают друг к другу.
//!
//! Сертификат сервера у всех комнат общий (один компьютер — один сервер),
//! иначе друзья, заходящие по адресу, видели бы предупреждение о смене
//! сертификата при каждой смене комнаты.

use std::path::{Path, PathBuf};

use anyhow::Context;
use serde::{Deserialize, Serialize};

/// Комната, которая была у хоста до появления списка комнат. Её код
/// приглашения лежит там же, где раньше, — у друзей он продолжает работать.
const FIRST_ROOM: &str = "default";

#[derive(Serialize, Deserialize, Clone, Debug)]
#[serde(rename_all = "camelCase")]
pub struct RoomConfig {
    pub id: String,
    pub name: String,
    /// Видна ли комната в списке «В локальной сети».
    #[serde(default = "yes")]
    pub lan: bool,
    /// Включать ли доступ через интернет при открытии.
    #[serde(default)]
    pub internet: bool,
}

fn yes() -> bool {
    true
}

pub struct Rooms {
    data_dir: PathBuf,
    list: Vec<RoomConfig>,
}

impl Rooms {
    /// Читает список комнат; если его ещё нет — одна комната `default_name`.
    pub fn load(data_dir: &Path, default_name: &str) -> Self {
        let list = std::fs::read(data_dir.join("rooms.json"))
            .ok()
            .and_then(|data| serde_json::from_slice::<Vec<RoomConfig>>(&data).ok())
            .filter(|list| !list.is_empty())
            .unwrap_or_else(|| {
                vec![RoomConfig {
                    id: FIRST_ROOM.into(),
                    name: default_name.into(),
                    lan: true,
                    internet: false,
                }]
            });
        Self { data_dir: data_dir.to_path_buf(), list }
    }

    pub fn list(&self) -> Vec<RoomConfig> {
        self.list.clone()
    }

    pub fn get(&self, id: &str) -> Option<RoomConfig> {
        self.list.iter().find(|r| r.id == id).cloned()
    }

    /// Файл с номером комнаты для кода приглашения.
    pub fn invite_file(&self, id: &str) -> PathBuf {
        if id == FIRST_ROOM {
            self.data_dir.join("room_id.txt")
        } else {
            self.data_dir.join("rooms").join(format!("{id}.txt"))
        }
    }

    pub fn create(&mut self, name: &str) -> anyhow::Result<RoomConfig> {
        let room = RoomConfig {
            id: vwf_nat::new_room_id()[..8].iter().map(|b| format!("{b:02x}")).collect(),
            name: clean_name(name)?,
            lan: true,
            internet: false,
        };
        self.list.push(room.clone());
        self.save()?;
        Ok(room)
    }

    /// Меняет комнату `id` (название, видимость, интернет) и сохраняет список.
    pub fn update(
        &mut self,
        id: &str,
        change: impl FnOnce(&mut RoomConfig),
    ) -> anyhow::Result<RoomConfig> {
        let room = self.list.iter_mut().find(|r| r.id == id).context("комната не найдена")?;
        change(room);
        room.name = clean_name(&room.name)?;
        let room = room.clone();
        self.save()?;
        Ok(room)
    }

    /// Удаляет комнату вместе с её кодом приглашения: старый код перестанет
    /// работать, даже если создать комнату с тем же названием.
    pub fn delete(&mut self, id: &str) -> anyhow::Result<()> {
        anyhow::ensure!(self.list.len() > 1, "последнюю комнату удалить нельзя");
        let before = self.list.len();
        self.list.retain(|r| r.id != id);
        anyhow::ensure!(self.list.len() < before, "комната не найдена");
        let _ = std::fs::remove_file(self.invite_file(id));
        self.save()
    }

    fn save(&self) -> anyhow::Result<()> {
        std::fs::create_dir_all(&self.data_dir)?;
        let data = serde_json::to_vec_pretty(&self.list)?;
        std::fs::write(self.data_dir.join("rooms.json"), data)
            .context("не удалось сохранить список комнат")
    }
}

fn clean_name(name: &str) -> anyhow::Result<String> {
    let name: String = name.trim().chars().filter(|c| !c.is_control()).take(40).collect();
    anyhow::ensure!(!name.is_empty(), "у комнаты должно быть название");
    Ok(name)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn first_room_keeps_old_invite_and_list_survives_restart() {
        let dir = std::env::temp_dir().join(format!("vwf-rooms-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);

        let mut rooms = Rooms::load(&dir, "Моя комната");
        let first = rooms.list()[0].clone();
        assert_eq!(rooms.invite_file(&first.id), dir.join("room_id.txt"));

        let games = rooms.create("  Игры с Саней ").unwrap();
        assert_eq!(games.name, "Игры с Саней");
        assert_ne!(rooms.invite_file(&games.id), rooms.invite_file(&first.id));
        rooms.update(&games.id, |r| r.lan = false).unwrap();

        let again = Rooms::load(&dir, "другое имя");
        let names: Vec<_> = again.list().into_iter().map(|r| (r.name, r.lan)).collect();
        assert_eq!(names, [("Моя комната".into(), true), ("Игры с Саней".into(), false)]);

        let mut again = again;
        again.delete(&games.id).unwrap();
        assert!(again.delete(&first.id).is_err(), "последнюю не удалить");
        assert!(again.create("   ").is_err());
        let _ = std::fs::remove_dir_all(&dir);
    }
}
