pub mod gamedata;
pub mod models;
pub mod schema;

use self::models::{Game, GamePlayer, NewGame, NewGamePlayer, NewPlayer, Player};
use anyhow::{anyhow, Result};
use diesel::prelude::*;
use dotenvy::dotenv;
use gamedata::{GameData, Port, SlippiPlayer};
use peppi::game::immutable;
use peppi::io::slippi;
use serde_json::{map, value};
use std::any::type_name;
use std::env;
use std::fs;
use std::io::{self, Write};
use std::string;

pub fn parse_replays() -> Result<()> {
    let test = false;

    let mut playtime_s = 0;
    let mut playtime_m = 0;

    let dir_path = env::current_dir()?;
    let dir = fs::read_dir(dir_path)?;

    for sub_dir in dir {
        let sub_dir = sub_dir?;

        let sub_dir_path = sub_dir.path();

        if sub_dir_path.is_dir() {
            if let Some(name) = sub_dir_path.file_name() {
                if name == "target"
                    || name == "src"
                    || name == "migrations"
                    || name.to_string_lossy().starts_with('.')
                {
                    continue;
                }
            }

            let mut gamecount: usize = 1;

            for replay in fs::read_dir(sub_dir_path)? {
                let replay = replay?;

                let replay_path = replay.path();
                let mut r = io::BufReader::new(fs::File::open(replay_path)?);

                let game = slippi::read(&mut r, None)?;

                if test {
                    println!("{:?}", game);
                }

                let gamedata = GameData::new_gamedata(&game);

                let gamedata = match gamedata {
                    Ok(g) => g,
                    Err(e) => {
                        println!("Error parsing gamedata: {}", e);
                        continue;
                    }
                };

                let mut gametime_s = gamedata.time();
                let gametime_m = gametime_s / 60;
                gametime_s = gametime_s % 60;
                playtime_s += gametime_s;
                playtime_m += gametime_m;

                println!(
                    "--game {} played on {} for {}:{}--",
                    gamecount,
                    gamedata.stage(),
                    gametime_m,
                    gametime_s
                );

                for player in gamedata.placements() {
                    if let Some(p) = player {
                        println!("{}: {} - {}", p.port(), p.code(), p.character());
                    }
                }

                println!("");

                gamecount += 1;

                if test {
                    println!("{}", type_of(game));
                    break;
                }
            }
        }
    }

    let playtime_h = playtime_m / 60;
    playtime_m = playtime_m % 60;

    println!(
        "total playtime: {}:{}:{}",
        playtime_h, playtime_m, playtime_s
    );
    Ok(())
}

pub fn establish_connection() -> Result<SqliteConnection> {
    dotenv().ok();

    let database_url = env::var("DATABASE_URL").map_err(|_| anyhow!("no database url found"))?;

    SqliteConnection::establish(&database_url)
        .map_err(|e| anyhow!("Error connecting to {}", database_url))
}

pub fn post_player(conn: &mut SqliteConnection, slippi_player: &SlippiPlayer) -> Result<Player> {
    use crate::schema::player;

    let new_player = NewPlayer {
        netplay: slippi_player.netplay(),
        code: slippi_player.code(),
    };

    diesel::insert_into(player::table)
        .values(&new_player)
        .returning(Player::as_returning())
        .get_result(conn)
        .map_err(|_| anyhow!("unable to post player"))
}

pub fn post_game_player(
    conn: &mut SqliteConnection,
    slippi_player: &SlippiPlayer,
) -> Result<GamePlayer> {
    use crate::schema::gamePlayer;

    post_player(conn, slippi_player)?;

    let new_game_player = NewGamePlayer {
        netplay: slippi_player.netplay(),
        character: slippi_player.character().into(),
        port: slippi_player.port().into(),
    };

    diesel::insert_into(gamePlayer::table)
        .values(&new_game_player)
        .returning(GamePlayer::as_returning())
        .get_result(conn)
        .map_err(|_| anyhow!("unable to post game player"))
}

pub fn post_game(conn: &mut SqliteConnection, gamedata: &GameData) -> Result<Game> {
    use crate::schema::game;

    let mut placements = gamedata.placements.iter().map(|p| {
        p.as_ref()
            .map(|p| post_game_player(conn, p).ok())
            .flatten()
            .map(|player| player.id)
    });
    let first = placements.next().unwrap();
    let second = placements.next().unwrap();
    let third = placements.next().unwrap();
    let fourth = placements.next().unwrap();
    let stage = gamedata.stage();

    let new_game = NewGame {
        first,
        second,
        third,
        fourth,
        stage: stage,
        time: gamedata.time(),
    };

    diesel::insert_into(game::table)
        .values(&new_game)
        .returning(Game::as_returning())
        .get_result(conn)
        .map_err(|_| anyhow!("unable to post game player"))
}

fn type_of<T>(_: T) -> &'static str {
    type_name::<T>()
}
