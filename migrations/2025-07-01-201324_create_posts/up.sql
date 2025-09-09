-- Table: player
CREATE TABLE player (
    netplay TEXT PRIMARY KEY NOT NULL,
    code TEXT NOT NULL
);

CREATE TABLE gamePlayer (
    id INTEGER PRIMARY KEY AUTOINCREMENT NOT NULL,
    netplay TEXT NOT NULL,
    character INTEGER NOT NULL CHECK (character >= 0 AND character <= 32),
    port INTEGER NOT NULL CHECK (port >= 0 AND port <= 3),

    UNIQUE(netplay, character, port),
    FOREIGN KEY (netplay) REFERENCES player(netplay)
);

-- Table: game
CREATE TABLE game (
    id INTEGER PRIMARY KEY AUTOINCREMENT NOT NULL,

    first INTEGER,
    second INTEGER,
    third INTEGER,
    fourth INTEGER,

    stage INTEGER NOT NULL CHECK(stage >= 0 AND stage <= 32),

    time INTEGER NOT NULL CHECK (time >= 0),

    -- At least one player must be non-null
    CHECK (
        first IS NOT NULL OR
        second IS NOT NULL OR
        third IS NOT NULL OR
        fourth IS NOT NULL
    ),

    -- Foreign key references to player table
    FOREIGN KEY (first) REFERENCES gamePlayer(id),
    FOREIGN KEY (second) REFERENCES gamePlayer(id),
    FOREIGN KEY (third) REFERENCES gamePlayer(id),
    FOREIGN KEY (fourth) REFERENCES gamePlayer(id)
);

CREATE TABLE stage (
    id INTEGER PRIMARY KEY AUTOINCREMENT NOT NULL CHECK (id >= 0 AND id <= 32),
    name TEXT NOT NULL
);

CREATE TABLE character (
    id INTEGER PRIMARY KEY AUTOINCREMENT NOT NULL CHECK (id >= 0 AND id <= 32),
    name TEXT NOT NULL
);