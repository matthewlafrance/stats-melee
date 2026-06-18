#!/bin/bash

# Path to your SQLite database file
DB_FILE="stats_melee.db"

# Run deletion queries
sqlite3 "$DB_FILE" <<EOF
DELETE FROM game;
DELETE FROM player;
DELETE FROM gamePlayer;
EOF

echo "All rows deleted from game, player, and gamePlayer tables."

