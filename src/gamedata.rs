use anyhow::{anyhow, Result};
use peppi;
use peppi::game::immutable::Game;
use serde_json::{map, value};
use soccer::{Display, Into, TryFrom};
use std::any::type_name;
use std::fmt;
use std::result::Result::Err;
use std::string;

/*
#[derive(Clone, Copy, Debug, PartialEq, Eq, TryFrom, Into)]
#[repr(u16)]
pub enum Stage {
    Dummy,
    Test,
    FountainOfDreams,
    PokemonStadium,
    PrincessPeachsCastle,
    KongoJungle,
    Brinstar,
    Corneria,
    YoshisStory,
    Onett,
    MuteCity,
    RainbowCruise,
    JungleJapes,
    GreatBay,
    HyruleTemple,
    BrinstarDepths,
    YoshisIsland,
    GreenGreens,
    Fourside,
    MushroomKingdomI,
    MushroomKingdomII,
    Akaneia,
    Venom,
    PokeFloats,
    BigBlue,
    IcicleMountain,
    Icetop,
    FlatZone,
    DreamLandN64,
    YoshisIslandN64,
    KongoJungleN64,
    Battlefield,
    FinalDestination,
}

impl fmt::Display for Stage {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let name = match self {
            Stage::Dummy => "Dummy",
            Stage::Test => "Test",
            Stage::FountainOfDreams => "Fountain Of Dreams",
            Stage::PokemonStadium => "Pokemon Stadium",
            Stage::PrincessPeachsCastle => "Princess Peach's Castle",
            Stage::KongoJungle => "Kongo Jungle",
            Stage::Brinstar => "Brinstar",
            Stage::Corneria => "Corneria",
            Stage::YoshisStory => "Yoshis Story",
            Stage::Onett => "Onett",
            Stage::MuteCity => "Mute City",
            Stage::RainbowCruise => "Rainbow Cruise",
            Stage::JungleJapes => "Jungle Japes",
            Stage::GreatBay => "Great Bay",
            Stage::HyruleTemple => "Hyrule Temple",
            Stage::BrinstarDepths => "Brinstar Depths",
            Stage::YoshisIsland => "Yoshis Island",
            Stage::GreenGreens => "Green Greens",
            Stage::Fourside => "Fourside",
            Stage::MushroomKingdomI => "Mushroom Kingdom I",
            Stage::MushroomKingdomII => "Mushroom Kingdom II",
            Stage::Akaneia => "Akaneia",
            Stage::Venom => "Venom",
            Stage::PokeFloats => "Poke Floats",
            Stage::BigBlue => "Big Blue",
            Stage::IcicleMountain => "Icicle Mountain",
            Stage::Icetop => "Icetop",
            Stage::FlatZone => "Flat Zone",
            Stage::DreamLandN64 => "Dream Land N64",
            Stage::YoshisIslandN64 => "Yoshis Island N64",
            Stage::KongoJungleN64 => "Kongo Jungle N64",
            Stage::Battlefield => "Battlefield",
            Stage::FinalDestination => "Final Destination",
        };
        write!(f, "{}", name)
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, TryFrom, Into)]
#[repr(i32)]
pub enum Character {
    Mario,
    Fox,
    CaptainFalcon,
    DonkeyKong,
    Kirby,
    Bowser,
    Link,
    Sheik,
    Ness,
    Peach,
    Popo,
    Nana,
    Pikachu,
    Samus,
    Yoshi,
    Jigglypuff,
    Mewtwo,
    Luigi,
    Marth,
    Zelda,
    YoungLink,
    DrMario,
    Falco,
    Pichu,
    GameAndWatch,
    Ganondorf,
    Roy,
    MasterHand,
    CrazyHand,
    WireFrameMale,
    WireFrameFemale,
    GigaBowser,
    Sandbag,
}

impl fmt::Display for Character {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let name = match self {
            Character::Mario => "Mario",
            Character::Fox => "Fox",
            Character::CaptainFalcon => "CaptainFalcon",
            Character::DonkeyKong => "DonkeyKong",
            Character::Kirby => "Kirby",
            Character::Bowser => "Bowser",
            Character::Link => "Link",
            Character::Sheik => "Sheik",
            Character::Ness => "Ness",
            Character::Peach => "Peach",
            Character::Popo => "Popo",
            Character::Nana => "Nana",
            Character::Pikachu => "Pikachu",
            Character::Samus => "Samus",
            Character::Yoshi => "Yoshi",
            Character::Jigglypuff => "Jigglypuff",
            Character::Mewtwo => "Mewtwo",
            Character::Luigi => "Luigi",
            Character::Marth => "Marth",
            Character::Zelda => "Zelda",
            Character::YoungLink => "YoungLink",
            Character::DrMario => "DrMario",
            Character::Falco => "Falco",
            Character::Pichu => "Pichu",
            Character::GameAndWatch => "GameAndWatch",
            Character::Ganondorf => "Ganondorf",
            Character::Roy => "Roy",
            Character::MasterHand => "MasterHand",
            Character::CrazyHand => "CrazyHand",
            Character::WireFrameMale => "WireFrameMale",
            Character::WireFrameFemale => "WireFrameFemale",
            Character::GigaBowser => "GigaBowser",
            Character::Sandbag => "Sandbag",
        };
        write!(f, "{}", name)
    }
}
*/

#[derive(Debug)]
pub struct GameData {
    pub placements: [Option<SlippiPlayer>; 4],
    pub stage: i32,
    pub time: i32,
}

impl GameData {
    pub fn new_gamedata(game: &peppi::game::immutable::Game) -> Result<GameData> {
        let metadata = game.metadata.as_ref().ok_or(anyhow!("no metadata found"))?;
        let placements = [
            SlippiPlayer::new_slippi_player(metadata, Port::P0),
            SlippiPlayer::new_slippi_player(metadata, Port::P1),
            SlippiPlayer::new_slippi_player(metadata, Port::P2),
            SlippiPlayer::new_slippi_player(metadata, Port::P3),
        ];
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

        // let character = Character::try_from(character.parse::<i32>().ok()?).ok()?;

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

fn type_of<T>(_: T) -> &'static str {
    type_name::<T>()
}
