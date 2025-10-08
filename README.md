# blob-e2e-poc

- AD (Anchored Datasystem) server
    - endpoints to create new 'counter' and increase it
    - stores in a sqlite DB
    - generates a POD proving each counter update
    - sends the POD proof in an ethereum blob tx
- Synchronizer server
    - follows beacon slots continuously looking for AD blobs
        - AD Init: defines a new AD and stores it in the DB by id
        - AD Update: updates an AD state based on the registered custom predicate, verifies the transition proof and stores the new state
    - endpoint to query AD last state
- integration test (bash script)
    - starts locally the AD & Synchronizer servers
    - queries the AD server to create a new counter, increase it, and fetches the latest state from the Synchronizer


## Usage
### Requirements
Required software: [curl](https://curl.se), [git](https://git-scm.com), [rust](https://rust-lang.org), [go](https://go.dev), [tmux](https://github.com/tmux/tmux), [jq](https://github.com/jqlang/jq).

Copy the `.env.default` file into `.env`, and set the `PRIV_KEY` and `RPC_URL` values.

### Run
To run the AD-Server & Synchronizer, together with a bash script that interacts with both, run the following command:
- `./full-flow.sh`

To run them manually:
- AD-Server: `RUST_LOG=ad_server=debug cargo run --release -p ad-server`
- Synchronizer: `RUST_LOG=synchronizer=debug cargo run --release -p synchronizer`
