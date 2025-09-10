use self::models::*;
use anyhow::{anyhow, Result};
use diesel::prelude::*;
use stats_melee::*;
use std::io::{self, Write};
use peppi::io::slippi::read;
use std::fs;

fn main() -> Result<()> {
    println!("analyzing games...");

    let mut connection = establish_connection()?;

    parse_replays()?;

    print!("press enter to continue");
    io::stdout().flush()?;
    let mut enter = String::new();
    io::stdin().read_line(&mut enter)?;
    Ok(())
}
