# API

## AD Server

```
# Init a new counter
curl -X POST http://0.0.0.0:8000/counter

# Update a counter
curl --json '5' http://0.0.0.0:8000/counter/2

# Get counter state
curl http://0.0.0.0:8000/counter/2
```

## Synchronizer

```
# Get AD state
curl http://0.0.0.0:8000/ad_state/0x123...
```
