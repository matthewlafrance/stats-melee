pub mod gamedata;
pub mod models;
pub mod schema;

use self::models::{Game, GamePlayer, NewGame, NewGamePlayer, NewPlayer, Player, Stage, NewStage, Character, NewCharacter};
use anyhow::{anyhow, Result};
use diesel::prelude::*;
use diesel::dsl;
use dotenvy::dotenv;
use gamedata::{GameData, Port, SlippiPlayer};
use peppi::game::immutable;
use peppi::io::slippi;
use serde_json::{map, value};
use std::any::type_name;
use std::{env, fs, string};
use std::io::{self, Write};


pub fn parse_replays() -> Result<()> {
    let test = false;

    /*
    let mut playtime_s = 0;
    let mut playtime_m = 0;
    */

    let dir_path = env::current_dir()?;
    let dir = fs::read_dir(dir_path)?;

    let mut connection = establish_connection()?;
    let games_empty = is_games_empty(&mut connection)?;
    let last_game_added = fs::metadata(database_url()?)?.modified()?;

    let newline = true;
    let netcode = prompt_user("enter netcode", newline)?;
    let netcode = netcode.trim_end();

    println!("analyzing games for {}...", netcode);

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
                let replay_system_time = replay.metadata()?.created()?;

                // println!("replay: {:?} - last game: {:?} - {}", replay_system_time, last_game_added, replay_system_time > last_game_added);

                // adds new replays to db
                if games_empty || replay_system_time > last_game_added {

                    let mut r = io::BufReader::new(fs::File::open(replay_path)?);
                    let game = slippi::read(&mut r, None)?;
                    let metadata = &game.metadata;
                    let gamedata = GameData::new_gamedata(&game)?;
                    let players = gamedata.placements();

                    post_game(&mut connection, &gamedata)?;


                    if test {
                        println!("{:?}", game);
                    }

                    println!("adding game to db...");

                    // post game to db here



                    /*
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
                    
                    */

                    if test {
                        println!("{}", type_of(game));
                        break;
                    }
                }

                // query db and print results
            }
        }
    }

    /*
    let playtime_h = playtime_m / 60;
    playtime_m = playtime_m % 60;

    println!(
        "total playtime: {}:{}:{}",
        playtime_h, playtime_m, playtime_s
    );
    */

    Ok(())
}

pub fn establish_connection() -> Result<SqliteConnection> {
    dotenv().ok();

    let database_url = database_url()?;

    SqliteConnection::establish(&database_url)
        .map_err(|e| anyhow!("Error connecting to {}", database_url))
}

pub fn post_player(conn: &mut SqliteConnection, slippi_player: &SlippiPlayer) -> Result<Player> {
    use crate::schema::player;

    let new_player = NewPlayer {
        netplay: slippi_player.netplay(),
        code: slippi_player.code(),
    };

    println!("herro");
    /*
    diesel::insert_or_ignore_into(player::table)
        .values(&new_player)
        .on_conflict(player::code)
        .do_nothing()
        .returning(Player::as_returning())
        .get_result(conn)
        .map_err(|e| anyhow!(e.to_string()))
    */

    Ok(insert_or_get_player(conn, &new_player)?)
}

pub fn post_game_player(conn: &mut SqliteConnection, slippi_player: &SlippiPlayer) -> Result<GamePlayer> {
    use crate::schema::gamePlayer;

    post_player(conn, slippi_player)?;


    let new_game_player = NewGamePlayer {
        netplay: slippi_player.netplay(),
        character: slippi_player.character().into(),
        port: slippi_player.port().into(),
    };

    // FIX THE GET RESULT INTO EXECUTE
    /*
    diesel::insert_or_ignore_into(gamePlayer::table)
        .values(&new_game_player)
        .on_conflict(gamePlayer::code)
        .do_nothing()
        .returning(GamePlayer::as_returning())
        .get_result(conn)
        .map_err(|e| anyhow!(e.to_string()))
    */

    Ok(insert_or_get_game_player(conn, &new_game_player)?)
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
        .map_err(|e| anyhow!(e.to_string()))
}

pub fn post_stage(conn: &mut SqliteConnection, name: String) -> Result<Stage> {
    use crate::schema::stage;

    let new_stage = NewStage{ name };

    diesel::insert_into(stage::table)
        .values(&new_stage)
        .returning(Stage::as_returning())
        .get_result(conn)
        .map_err(|e| anyhow!(e.to_string()))
}

pub fn post_character(conn: &mut SqliteConnection, name: String) -> Result<Character> {
    use crate::schema::character;

    let new_character = NewCharacter{ name };

    diesel::insert_into(character::table)
        .values(&new_character)
        .returning(Character::as_returning())
        .get_result(conn)
        .map_err(|e| anyhow!(e.to_string()))
}

pub fn insert_or_get_player(conn: &mut SqliteConnection, new_player: &NewPlayer) -> diesel::result::QueryResult<Player> {
    use crate::schema::player::dsl::*;

    if let Some(inserted) = diesel::insert_into(player)
        .values(new_player)
        .on_conflict(netplay)
        .do_nothing()
        .returning(Player::as_returning())
        .get_result(conn)
        .optional()? 
    {
        Ok(inserted)
    } else {
        player
            .filter(netplay.eq(&new_player.netplay))
            .first::<Player>(conn)
    }
}

pub fn insert_or_get_game_player(conn: &mut SqliteConnection, new_game_player: &NewGamePlayer) -> diesel::result::QueryResult<GamePlayer> {
    use crate::schema::gamePlayer::dsl::*;

    if let Some(inserted) = diesel::insert_into(gamePlayer)
        .values(new_game_player)
        .on_conflict((netplay, character, port))
        .do_nothing()
        .returning(GamePlayer::as_returning())
        .get_result(conn)
        .optional()? 
    {
        Ok(inserted)
    } else {
        gamePlayer
            .filter(netplay.eq(&new_game_player.netplay))
            .filter(character.eq(new_game_player.character))
            .filter(port.eq(new_game_player.port))
            .first::<GamePlayer>(conn)
    }
}


pub fn is_games_empty(conn: &mut SqliteConnection) -> Result<bool> {

    use crate::schema::game::dsl::*;

    let count: i64 = game.select(dsl::count_star()).first(conn)?;
    
    Ok(count == 0)
}

pub fn prompt_user(prompt: &str, newline: bool) -> Result<String> {

    if newline {
        println!("{}", prompt);
    } else {
        print!("{}", prompt);
    }

    io::stdout().flush()?;
    let mut response = String::new();
    io::stdin().read_line(&mut response)?;
    Ok(response)
}

pub fn database_url() -> Result<String> {
    env::var("DATABASE_URL").map_err(|_| anyhow!("no database url found")) 
}

fn type_of<T>(_: T) -> &'static str {
    type_name::<T>()
}
