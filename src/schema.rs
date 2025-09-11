// @generated automatically by Diesel CLI.

diesel::table! {
    character (id) {
        id -> Integer,
        name -> Text,
    }
}

diesel::table! {
    game (id) {
        id -> Integer,
        first -> Nullable<Integer>,
        second -> Nullable<Integer>,
        third -> Nullable<Integer>,
        fourth -> Nullable<Integer>,
        stage -> Integer,
        time -> Integer,
    }
}

diesel::table! {
    gamePlayer (id) {
        id -> Integer,
        code -> Text,
        character -> Integer,
        port -> Integer,
    }
}

diesel::table! {
    player (code) {
        code -> Text,
        netplay -> Text,
    }
}

diesel::table! {
    stage (id) {
        id -> Integer,
        name -> Text,
    }
}

diesel::joinable!(gamePlayer -> player (code));

diesel::allow_tables_to_appear_in_same_query!(
    character,
    game,
    gamePlayer,
    player,
    stage,
);
