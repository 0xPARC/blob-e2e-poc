#!/usr/bin/env bash

echo -e "getting Beacon chain last slot number"
LAST_SLOT=$(curl -X GET https://ethereum-sepolia-beacon-api.publicnode.com/eth/v2/beacon/blocks/head | jq -r '.data.message.slot')

echo -e "updating AD_GENESIS_SLOT value in the .env file"
sed -i "s/^AD_GENESIS_SLOT=.*/AD_GENESIS_SLOT=\"$LAST_SLOT\"/" .env

echo -e "removing old db"
DB1_PATH=$(sed -n 's/^AD_SERVER_SQLITE_PATH="\([^"]*\)"/\1/p' .env)
DB2_PATH=$(sed -n 's/^SYNCHRONIZER_SQLITE_PATH="\([^"]*\)"/\1/p' .env)
rm $DB1_PATH
rm $DB2_PATH

echo -e "opening tmux with 3 panels"
tmux new-session -d -s fullflow
tmux split-window -v
tmux split-window -v
tmux select-layout even-vertical

# run the AnchoredDatasystem server
tmux send-keys -t fullflow:0.0 'cargo run --release -p ad-server' C-m

# run the Synchronizer server
tmux send-keys -t fullflow:0.1 'RUST_LOG=synchronizer=debug cargo run --release -p synchronizer' C-m

# leave ready the full-flow script command without executing it yet
tmux send-keys -t fullflow:0.2 'echo "INSTRUCTIONS: once the first two panels are already running the servers (AD & Synchronizer), execute the following command to run the integration test:"' C-m
tmux send-keys -t fullflow:0.2 './full-flow-aux.sh'


tmux select-pane -t fullflow:0.2

tmux attach -t fullflow
