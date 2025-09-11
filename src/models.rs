use crate::schema::{game, gamePlayer, player, stage, character};
use diesel::prelude::*;

#[derive(Queryable, Selectable, Debug)]
#[diesel(table_name = crate::schema::game)]
#[diesel(check_for_backend(diesel::sqlite::Sqlite))]
pub struct Game {
    pub id: i32,
    pub first: Option<i32>,
    pub second: Option<i32>,
    pub third: Option<i32>,
    pub fourth: Option<i32>,
    pub stage: i32,
    pub time: i32,
}

#[derive(Queryable, Selectable)]
#[diesel(table_name = crate::schema::player)]
#[diesel(check_for_backend(diesel::sqlite::Sqlite))]
pub struct Player {
    pub netplay: String,
    pub code: String,
}

#[derive(Queryable, Selectable)]
#[diesel(table_name = crate::schema::gamePlayer)]
#[diesel(check_for_backend(diesel::sqlite::Sqlite))]
pub struct GamePlayer {
    pub id: i32,
    pub code: String,
    pub character: i32,
    pub port: i32,
}

#[derive(Queryable, Selectable)]
#[diesel(table_name = crate::schema::stage)]
#[diesel(check_for_backend(diesel::sqlite::Sqlite))]
pub struct Stage {
    pub id: i32,
    pub name: String,
}

#[derive(Queryable, Selectable)]
#[diesel(table_name = crate::schema::character)]
#[diesel(check_for_backend(diesel::sqlite::Sqlite))]
pub struct Character {
    pub id: i32,
    pub name: String,
}

#[derive(Insertable)]
#[diesel(table_name = player)]
pub struct NewPlayer<'a> {
    pub code: &'a str,
    pub netplay: &'a str,
}

#[derive(Insertable)]
#[diesel(table_name = gamePlayer)]
pub struct NewGamePlayer<'a> {
    pub code: &'a str,
    pub character: i32,
    pub port: i32,
}

#[derive(Insertable)]
#[diesel(table_name = game)]
pub struct NewGame {
    pub first: Option<i32>,
    pub second: Option<i32>,
    pub third: Option<i32>,
    pub fourth: Option<i32>,
    pub stage: i32,
    pub time: i32,
}

#[derive(Insertable)]
#[diesel(table_name = stage)]
pub struct NewStage {
    pub id: i32,
    pub name: String,
}


#[derive(Insertable)]
#[diesel(table_name = character)]
pub struct NewCharacter {
    pub id: i32,
    pub name: String,
}