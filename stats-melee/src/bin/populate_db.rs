use anyhow::Result;
use stats_melee::*;
use stats_melee::gamedata::{STAGES, CHARACTERS};

fn main() -> Result<()> {
    let mut connection = establish_connection()?;

    println!("populating database with stage and character information");

    for (id, stage) in STAGES.iter().enumerate() {
        post_stage(&mut connection, id as i32, stage.to_string())?;
    }

    for (id, character) in CHARACTERS.iter().enumerate() {
        post_character(&mut connection, id as i32, character.to_string())?;
    }

    println!("database successfully populated");

    Ok(())
}
