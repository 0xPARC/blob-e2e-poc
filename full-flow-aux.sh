#!/usr/bin/env bash

echo "running full flow"

echo -e "creating new counter, response:"
curl -X POST http://0.0.0.0:8000/counter

echo -e "\ngetting counter, response:"
curl -X GET http://0.0.0.0:8000/counter/1

echo -e "\ncreating new counter, response:"
curl --json '5' http://0.0.0.0:8000/counter/1

echo -e "\ngetting counter, response:"
curl -X GET http://0.0.0.0:8000/counter/1

echo -e "\ngetting the state from the Synchronizer server"
curl -X GET http://0.0.0.0:8001/ad_state/0000000000000000000000000000000000000000000000000000000000000001

echo -e ""
