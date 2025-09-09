use anyhow::{anyhow, Result};
use peppi::{self, game};
use peppi::game::immutable::Game;
use serde_json::{map, value};
use soccer::{Display, Into, TryFrom};
use std::any::type_name;
use std::fmt;
use std::result::Result::Err;
use std::string;

#[derive(Debug)]
pub struct GameData {
    pub placements: [Option<SlippiPlayer>; 4],
    pub stage: i32,
    pub time: i32,
}

impl GameData {
    pub fn new_gamedata(game: &peppi::game::immutable::Game) -> Result<GameData> {
        let metadata = game.metadata.as_ref().ok_or(anyhow!("no metadata found"))?;
        let players = game.end.clone().ok_or(anyhow!("error"))?.players.ok_or(anyhow!("error"))?;
        let mut placements: [Option<SlippiPlayer>; 4] = [None, None, None, None];

        for (i, player) in players.iter().enumerate() {
            placements[i] = SlippiPlayer::new_slippi_player(metadata, match player.port {
                game::Port::P1 => Port::P0,
                game::Port::P2 => Port::P1,
                game::Port::P3 => Port::P2,
                game::Port::P4 => Port::P3,
            });
        }

        let stage = game.start.stage as i32;
        let time = Self::game_len(&game)?;

        Ok(GameData {
            placements,
            stage,
            time,
        })
    }

    pub fn placements(&self) -> &[Option<SlippiPlayer>; 4] {
        &self.placements
    }

    pub fn stage(&self) -> i32 {
        self.stage
    }

    pub fn time(&self) -> i32 {
        self.time
    }

    pub fn game_len(game: &Game) -> Result<i32> {
        (game.frames.len() / 60)
            .try_into()
            .map_err(|e| anyhow!("can't parse game length"))
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, TryFrom, Into, Display)]
#[repr(i32)]
pub enum Port {
    P0,
    P1,
    P2,
    P3,
}

#[derive(Debug)]
pub struct SlippiPlayer {
    pub netplay: String,
    pub code: String,
    pub character: i32,
    pub port: Port,
}

impl SlippiPlayer {
    pub fn new_slippi_player(
        metadata: &map::Map<string::String, value::Value>,
        port: Port,
    ) -> Option<SlippiPlayer> {
        let port_int: i32 = port.into();
        let port_string = port.to_string();

        let netplay = match &metadata["players"][&port_string]["names"]["netplay"] {
            value::Value::String(n) => n.clone(),
            _ => {
                return None;
            }
        };

        let code = match &metadata["players"][&port_string]["names"]["code"] {
            value::Value::String(c) => c.clone(),
            _ => {
                return None;
            }
        };

        let characters = &metadata["players"][&port_string]["characters"].as_object();

        let characters = match characters {
            Some(c) => c,
            None => {
                return None;
            }
        };

        let mut character = None;
        let mut frames = 0;
        for c in characters.keys() {
            let current_frames = characters.get(c)?.as_u64()?;
            if current_frames > frames {
                character = Some(c);
                frames = current_frames;
            }
        }

        let character = match character {
            Some(c) => c,
            None => {
                return None;
            }
        };

        let character = character.parse::<i32>().ok()?;

        Some(SlippiPlayer {
            netplay,
            code,
            character,
            port,
        })
    }

    pub fn netplay(&self) -> &str {
        &self.netplay
    }

    pub fn code(&self) -> &str {
        &self.code
    }

    pub fn character(&self) -> i32 {
        self.character
    }

    pub fn port(&self) -> Port {
        self.port
    }
}

pub fn type_of<T>(_: T) -> &'static str {
    type_name::<T>()
}
