pub mod gamedata;
pub mod analytics;
pub mod models;
pub mod schema;


use self::models::{Game, GamePlayer, NewGame, NewGamePlayer, NewPlayer, Player, Stage, NewStage, Character, NewCharacter};
use self::analytics::{WinProportion, WinAnalytics};
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
use std::collections::HashMap;

pub static NUM_STAGES: usize = 33;
pub static NUM_CHARACTERS: usize = 33;


pub fn parse_new_replays(conn: &mut SqliteConnection) -> Result<usize> {
    let test = false;

    let mut dir_path = env::current_dir()?;
    dir_path.push("..");
    let dir = fs::read_dir(dir_path)?;

    let games_empty = is_games_empty(conn)?;
    let db_modified = fs::metadata(database_url()?)?.modified()?;

    let mut gamecount: usize = 0;

    for sub_dir in dir {

        let sub_dir = sub_dir?;
        let sub_dir_path = sub_dir.path();

        if sub_dir_path.is_dir() {

            if let Some(name) = sub_dir_path.file_name() {
                if name == "target"
                    || name == "src"
                    || name == "migrations"
                    || name == "stats-melee"
                    || name.to_string_lossy().starts_with('.')
                {
                    continue;
                }
            }

            let sub_dir_modified = sub_dir.metadata()?.modified()?;

            if !games_empty && sub_dir_modified < db_modified {
                continue;
            }

            for replay in fs::read_dir(sub_dir_path)? {

                let replay = replay?;
                let replay_path = replay.path();
                let replay_created = replay.metadata()?.created()?;

                if games_empty || replay_created > db_modified {

                    let mut r = io::BufReader::new(fs::File::open(replay_path)?);
                    let game = slippi::read(&mut r, None)?;
                    let metadata = &game.metadata;
                    let gamedata = GameData::new_gamedata(&game)?;
                    let players = gamedata.placements();

                    post_game(conn, &gamedata)?;

                    gamecount += 1;

                }
            }
        }
    }

    Ok(gamecount)
}

pub fn filter_games(conn: &mut SqliteConnection, code: &str) -> Result<Vec<Game>> {

    use crate::schema::{game, gamePlayer};

    game::table
        .inner_join(
            gamePlayer::table.on(
                game::first
                    .eq(gamePlayer::id.nullable())
                    .or(game::second.eq(gamePlayer::id.nullable()))
                    .or(game::third.eq(gamePlayer::id.nullable()))
                    .or(game::fourth.eq(gamePlayer::id.nullable())),
            ),
        )
        .filter(gamePlayer::code.eq(code))
        .select(game::all_columns)
        .load::<Game>(conn).map_err(|e| anyhow!(e.to_string()))
}

pub fn analyze_games(conn: &mut SqliteConnection, games: &Vec<Game>, player_code: &str) -> Result<WinAnalytics> {

    let mut opponents: HashMap<String, WinProportion> = HashMap::new();
    let mut stages = [WinProportion::new_winproportion(); NUM_STAGES];
    let mut played_characters = [WinProportion::new_winproportion(); NUM_CHARACTERS];
    let mut opp_characters = [WinProportion::new_winproportion(); NUM_CHARACTERS];

    for game in games {

        let mut codes = [None, None, None, None];
        let mut characters = [None, None, None, None];
        
        if let Some(f) = game.first {
            codes[0] = Some(get_game_player_code(conn, f)?);
            characters[0] = Some(get_character(conn, f)?);
        }

        if let Some(s) = game.second {
            codes[1] = Some(get_game_player_code(conn, s)?);
            characters[1] = Some(get_character(conn, s)?);
        }

        if let Some(t) = game.third {
            codes[2] = Some(get_game_player_code(conn, t)?);
            characters[2] = Some(get_character(conn, t)?);
        }

        if let Some(f) = game.fourth {
            codes[3] = Some(get_game_player_code(conn, f)?);
            characters[3] = Some(get_character(conn, f)?);
        }

        let player_won = codes[0] == Some(player_code.to_string());

        for (code_option, character_option) in codes.iter().zip(characters.iter()) {
            if let Some(code) = code_option {
                let character = character_option.ok_or(anyhow!("no character found for player"))?;

                if code != player_code {
                    let opps_winproportion = opponents.entry(code.to_string()).or_insert(WinProportion::new_winproportion());

                    if player_won {
                        opps_winproportion.wins += 1;
                        opp_characters[character as usize].wins += 1;
                    }

                    opps_winproportion.total += 1;
                    opp_characters[character as usize].total += 1;
                } else {

                    if player_won {
                        played_characters[character as usize].wins += 1;
                    }

                    played_characters[character as usize].total += 1;
                }
            }
        }

        if player_won {
            stages[game.stage as usize].wins += 1;
        }

        stages[game.stage as usize].total += 1;
    }

    for opp_winproportion in opponents.values_mut() {
        opp_winproportion.update_proportion();
    }

    for i in 0..NUM_STAGES {
        stages[i].update_proportion();
    }

    for i in 0..NUM_CHARACTERS {
        played_characters[i].update_proportion();
        opp_characters[i].update_proportion();
    }

    Ok(WinAnalytics {
        opponents,
        stages,
        played_characters,
        opp_characters,
    })
}

pub fn get_game_player_code(conn: &mut SqliteConnection, find_id: i32) -> Result<String> {
    use crate::schema::gamePlayer::dsl::*;

    gamePlayer.filter(id.eq(find_id)).select(code).first::<String>(conn).map_err(|e| anyhow!(e.to_string()))
}

pub fn get_character(conn: &mut SqliteConnection, find_id: i32) -> Result<i32> {
    use crate::schema::gamePlayer::dsl::*;

    gamePlayer.filter(id.eq(find_id)).select(character).first::<i32>(conn).map_err(|e| anyhow!(e.to_string()))
}


pub fn post_player(conn: &mut SqliteConnection, slippi_player: &SlippiPlayer) -> Result<Player> {
    use crate::schema::player;

    let new_player = NewPlayer {
        netplay: slippi_player.netplay(),
        code: slippi_player.code(),
    };

    Ok(insert_or_get_player(conn, &new_player)?)
}

pub fn post_game_player(conn: &mut SqliteConnection, slippi_player: &SlippiPlayer) -> Result<GamePlayer> {
    use crate::schema::gamePlayer;

    post_player(conn, slippi_player)?;


    let new_game_player = NewGamePlayer {
        code: slippi_player.code(),
        character: slippi_player.character().into(),
        port: slippi_player.port().into(),
    };

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

pub fn post_stage(conn: &mut SqliteConnection, id: i32, name: String) -> Result<Stage> {
    use crate::schema::stage;

    let new_stage = NewStage{ id, name };

    diesel::insert_into(stage::table)
        .values(&new_stage)
        .returning(Stage::as_returning())
        .get_result(conn)
        .map_err(|e| anyhow!(e.to_string()))
}

pub fn post_character(conn: &mut SqliteConnection, id: i32, name: String) -> Result<Character> {
    use crate::schema::character;

    let new_character = NewCharacter{ id, name };

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
        .on_conflict(code)
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
        .on_conflict((code, character, port))
        .do_nothing()
        .returning(GamePlayer::as_returning())
        .get_result(conn)
        .optional()? 
    {
        Ok(inserted)
    } else {
        gamePlayer
            .filter(code.eq(&new_game_player.code))
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
    let response = response.trim_end().to_owned();
    Ok(response)
}

pub fn establish_connection() -> Result<SqliteConnection> {
    dotenv().ok();

    let database_url = database_url()?;

    SqliteConnection::establish(&database_url)
        .map_err(|e| anyhow!("Error connecting to {}", database_url))
}

pub fn database_url() -> Result<String> {
    env::var("DATABASE_URL").map_err(|_| anyhow!("no database url found")) 
}

fn type_of<T>(_: T) -> &'static str {
    type_name::<T>()
}
