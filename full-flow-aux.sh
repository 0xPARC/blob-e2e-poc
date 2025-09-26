#!/usr/bin/env bash

echo "running full flow"

echo -e "creating new set, response:"
curl -X POST http://0.0.0.0:8000/set

echo -e "\ngetting set, response:"
curl -X GET http://0.0.0.0:8000/set/1

echo -e "\ninserting value into set, response:"
curl --json '{"Int": "33"}' http://0.0.0.0:8000/set/1

echo -e "\ngetting counter, response:"
curl -X GET http://0.0.0.0:8000/set/1

echo -e "\ngetting the state from the Synchronizer server"
curl -X GET http://0.0.0.0:8001/ad_state/0000000000000000000000000000000000000000000000000000000000000001

echo -e ""
