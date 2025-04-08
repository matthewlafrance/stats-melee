use std::env;
use std::{fs, io};
use std::any::type_name;
use peppi::io::slippi::read;
use peppi::game::immutable::Game;
use anyhow::{Result, Error};

fn main() -> Result<()> {
    let mut seconds = total_playtime()?;
    let mut minutes = seconds / 60;
    seconds = seconds % 60;
    let hours = minutes / 60;
    minutes = minutes % 60;

    println!("total playtime: {}:{}:{}", hours, minutes, seconds);
    Ok(())
}

fn total_playtime() -> Result<usize> {
    let dir_path = env::current_dir()?;
    let dir = fs::read_dir(dir_path)?;
    let mut total_playtime = 0;

    for sub_dir in dir {
        let sub_dir = sub_dir?;

        let sub_dir_path = sub_dir.path();
        
        if sub_dir_path.is_dir() {
            if let Some(name) = sub_dir_path.file_name() {
                if name == "target" || name == "src" || name.to_string_lossy().starts_with('.') {
                    continue;
                } 
            }


            for replay in fs::read_dir(sub_dir_path)? {
                let replay = replay?;

                let replay_path = replay.path();
                let mut r = io::BufReader::new(fs::File::open(replay_path)?);
                let game = read(&mut r, None)?;
                total_playtime += game_len(game);
            }
        } else {
            continue;
        }
    }
    Ok(total_playtime)
}

fn game_len(game: Game) -> usize {
    game.frames.len()/60
}

fn type_of<T>(_: T) -> &'static str {
    type_name::<T>()
}
