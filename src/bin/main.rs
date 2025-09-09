use self::models::*;
use anyhow::{anyhow, Result};
use diesel::prelude::*;
use melee_anal::*;
use std::io::{self, Write};

fn main() -> Result<()> {
    println!("analyzing games...");

    //let mut r = io::BufReader::new(fs::File::open("test_slps/test.slp")?);
    //let game = read(&mut r, None)?;
    //let metadata = game.metadata.unwrap();
    //println!("{:#?}", &metadata["players"]["0"]["characters"]);

    let mut connection = establish_connection()?;

    parse_replays()?;

    print!("press enter to continue");
    io::stdout().flush()?;
    let mut enter = String::new();
    io::stdin().read_line(&mut enter)?;
    Ok(())
}
