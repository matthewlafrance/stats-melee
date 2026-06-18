use anyhow::Result;
use stats_melee::*;
use std::env;
use std::path::PathBuf;

fn main() -> Result<()> {
    println!("parsing new replays...");

    let mut connection = establish_connection()?;

    // Default to the original behavior (sibling directories of stats-melee/),
    // but allow an override via the first CLI arg so we can point at any
    // replay root (e.g. test_slps/).
    let root: PathBuf = env::args()
        .nth(1)
        .map(PathBuf::from)
        .unwrap_or_else(|| {
            let mut p = env::current_dir().expect("current dir");
            p.push("..");
            p
        });

    let db_path = database_url()?;
    let new_games = parse_new_replays(&mut connection, &root, &db_path)?;
    println!("{} new replays added", new_games);

    let code = prompt_user("enter code: ", false)?;
    let games = filter_games(&mut connection, &code)?;
    let analytics = analyze_games(&mut connection, &games, &code)?;

    println!("{}", analytics);

    Ok(())
}