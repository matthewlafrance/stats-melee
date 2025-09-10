use self::models::*;
use anyhow::{anyhow, Result};
use diesel::prelude::*;
use stats_melee::*;
use std::io::{self, Write};

fn main() -> Result<()> {

    use self::schema::{stage, character};

    let mut connection = establish_connection()?;

    println!("populating database with stage and character information");

    let stages = [
        "Dummy",
        "Test",
        "FountainOfDreams",
        "PokemonStadium",
        "PrincessPeachsCastle",
        "KongoJungle",
        "Brinstar",
        "Corneria",
        "YoshisStory",
        "Onett",
        "MuteCity",
        "RainbowCruise",
        "JungleJapes",
        "GreatBay",
        "HyruleTemple",
        "BrinstarDepths",
        "YoshisIsland",
        "GreenGreens",
        "Fourside",
        "MushroomKingdomI",
        "MushroomKingdomII",
        "Akaneia",
        "Venom",
        "PokeFloats",
        "BigBlue",
        "IcicleMountain",
        "Icetop",
        "FlatZone",
        "DreamLandN64",
        "YoshisIslandN64",
        "KongoJungleN64",
        "Battlefield",
        "FinalDestination",
    ];

    let characters = [
        "Mario",
        "Fox",
        "CaptainFalcon",
        "DonkeyKong",
        "Kirby",
        "Bowser",
        "Link",
        "Sheik",
        "Ness",
        "Peach",
        "Popo",
        "Nana",
        "Pikachu",
        "Samus",
        "Yoshi",
        "Jigglypuff",
        "Mewtwo",
        "Luigi",
        "Marth",
        "Zelda",
        "YoungLink",
        "DrMario",
        "Falco",
        "Pichu",
        "GameAndWatch",
        "Ganondorf",
        "Roy",
        "MasterHand",
        "CrazyHand",
        "WireFrameMale",
        "WireFrameFemale",
        "GigaBowser",
        "Sandbag",
    ];

    for stage in stages {
        post_stage(&mut connection, stage.to_owned())?;
    }

    for character in characters {
        post_character(&mut connection, character.to_owned())?;
    }

    println!("database successfully populated");

    Ok(())
}
