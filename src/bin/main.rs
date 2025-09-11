use self::models::*;
use anyhow::{anyhow, Result};
use diesel::prelude::*;
use stats_melee::*;
use std::io::{self, Write};
use peppi::io::slippi::read;
use std::fs;

fn main() -> Result<()> {
    println!("parsing new replays...");

    let mut connection = establish_connection()?;

    let new_games = parse_new_replays(&mut connection)?;

    println!("{} new replays added", new_games);

    let code = prompt_user("enter code: ", false)?;

    let games = filter_games(&mut connection, &code)?;

    for game in games {
        println!("{:?}", game);
    }

    Ok(())
}
