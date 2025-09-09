use self::models::*;
use anyhow::{anyhow, Result};
use diesel::prelude::*;
use melee_anal::*;
use std::io::{self, Write};

fn main() -> Result<()> {
    //let mut r = io::BufReader::new(fs::File::open("test_slps/test.slp")?);
    //let game = read(&mut r, None)?;
    //let metadata = game.metadata.unwrap();
    //println!("{:#?}", &metadata["players"]["0"]["characters"]);

    // read_replays()?;

    use self::schema::{gamePlayer, player};

    let mut connection = establish_connection()?;

    println!("displaying all players");

    let players_result = player::table.load::<Player>(&mut connection);

    match players_result {
        Ok(players) => {
            for player in players {
                println!("{} -- {}", player.netplay, player.code);
            }
        }
        Err(e) => {
            eprintln!("Error loading players: {}", e);
        }
    }

    println!("");

    println!("displaying all game players");

    let game_players_result = gamePlayer::table.load::<GamePlayer>(&mut connection);

    match game_players_result {
        Ok(game_players) => {
            for game_player in game_players {
                println!(
                    "{} -- {} -- {} -- {}",
                    game_player.id, game_player.netplay, game_player.character, game_player.port
                );
            }
        }
        Err(e) => {
            eprintln!("Error loading players: {}", e);
        }
    }

    print!("press enter to continue");
    io::stdout().flush()?;
    let mut enter = String::new();
    io::stdin().read_line(&mut enter)?;
    Ok(())
}
