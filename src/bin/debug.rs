use self::models::*;
use anyhow::{anyhow, Result};
use diesel::prelude::*;
use melee_anal::*;
use std::io::{self, Write};
use peppi::io::slippi::read;
use std::fs;
use gamedata::type_of;

fn main() -> Result<()> {
    let mut r = io::BufReader::new(fs::File::open("test_slps/test2.slp").unwrap());
    let game = read(&mut r, None).unwrap();
    let game = game.end.unwrap().players.unwrap();
    for player in game {
        println!("{:#?}", type_of(player.port));
    }
    // let metadata = game.metadata.unwrap();
    // println!("{:#?}", &metadata["players"]["0"]["characters"]);
    Ok(())
}

/*
end: Some(
    End {
        method: Game,
        bytes: Bytes { len: 6 },
        lras_initiator: Some(
            None,
        ),
        players: Some(
            [
                PlayerEnd {
                    port: P1,
                    placement: 0,
                },
                PlayerEnd {
                    port: P2,
                    placement: 1,
                },
            ],
        ),
    },
),
*/