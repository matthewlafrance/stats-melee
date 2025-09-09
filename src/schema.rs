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
        netplay -> Text,
        character -> Integer,
        port -> Integer,
    }
}

diesel::table! {
    player (netplay) {
        netplay -> Text,
        code -> Text,
    }
}

diesel::table! {
    stage (id) {
        id -> Integer,
        name -> Text,
    }
}

diesel::joinable!(gamePlayer -> player (netplay));

diesel::allow_tables_to_appear_in_same_query!(
    character,
    game,
    gamePlayer,
    player,
    stage,
);
