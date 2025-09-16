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


## API endpoints

### AD Server

```
# Init a new counter
curl -X POST http://0.0.0.0:8000/counter

# Update a counter
curl --json '5' http://0.0.0.0:8000/counter/2

# Get counter state
curl http://0.0.0.0:8000/counter/2
```

### Synchronizer

```
# Get AD state
curl http://0.0.0.0:8000/ad_state/0x123...
```
