use self::gamedata::*;
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

    let mut connection = establish_connection()?;

    let frank = SlippiPlayer {
        netplay: "frnk#948".to_string(),
        code: "BigFrank70".to_string(),
        character: 32,
        port: Port::P0,
    };

    let buscuit = SlippiPlayer {
        netplay: "bisc#223".to_string(),
        code: "Buscuitlover92".to_string(),
        character: 1,
        port: Port::P1,
    };

    println!("adding dummy data");

    post_game_player(&mut connection, &frank);

    post_game_player(&mut connection, &buscuit);

    print!("press enter to continue");
    io::stdout().flush()?;
    let mut enter = String::new();
    io::stdin().read_line(&mut enter)?;
    Ok(())
}
