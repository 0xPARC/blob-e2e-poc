# blob-e2e-poc

- AD (Anchored Datasystem) server
    - endpoints to create new 'membership list' and append users to it and get the list
    - stores in a sqlite DB
    - generates a POD proving each membership list update
    - sends the POD proof in an ethereum blob tx
- Synchronizer server
    - follows beacon slots continuously looking for AD blobs
        - AD Init: defines a new AD and stores it in the DB by id
        - AD Update: updates an AD state based on the registered custom predicate, verifies the transition proof and stores the new state
    - endpoint to query AD last state
- integration test (bash script)
    - starts locally the AD & Synchronizer servers
    - queries the AD server to create a new membership list, append users to it, and fetches the latest state from the Synchronizer


## Usage
### Requirements
Required software: [curl](https://curl.se), [git](https://git-scm.com), [rust](https://rust-lang.org), [go](https://go.dev), [tmux](https://github.com/tmux/tmux), [jq](https://github.com/jqlang/jq).

Copy the `.env.default` file into `.env`, and set the `PRIV_KEY` (corresponding to an address which holds some Sepolia ETH) and `RPC_URL` values.

### Run
Once having the `.env` file ready with the `PRIV_KEY` and `RPC_URL` properly filled, to run the artifacts generation, and the AD-Server & Synchronizer, together with a bash script that interacts with both, run the following command:
- `./full-flow.sh`

This will generate all the needed files, and it will open a new tmux session with 3 panels; one for the AD-Server, one for the Synchronizer, and one to run the `full-flow-requests.sh` file which acts as a client.

Alternatively can run manually the commands that appear in the `full-flow.sh` file.

When running the system, there are operations that take some time:
(numbers from a AMD Ryzen 5 5900-XT 16-Core)
- First time run needs to generate the Groth16 trusted setup: `4m`
- Each run:
    - Loading Groth16 pk: `~40s` (this will be removed)
    - Full POD update:
        - Proof generation: `21.1s`
            - prove mainpod update: `6.5s`
            - shrink circuit (plus making it groth16 friendly): `3s`
            - Groth16 prove: `11.6s`
        - Tx inclusion (from AD-Server to blockchain): 1 - 60s, assume an average of `30s`
        - Blob synchronizing (Synchronizer): `<2s`
