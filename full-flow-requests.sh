#!/usr/bin/env bash

echo "running full flow"

echo -e "creating new membership_list, response:"
./client.sh --wait-complete membership_list_create

echo -e "\ngetting membership_list, response:"
./client.sh --wait-complete membership_list_get 1

echo -e "\ninit membership_list, response:"
./client.sh --wait-complete membership_list_update 1 '"init"'

echo -e "\ngetting membership_list, response:"
./client.sh --wait-complete membership_list_get 1

echo -e "\nadd to membership_list, response:"
./client.sh --wait-complete membership_list_update 1 '{"add":{"group":"blue","user":"alice"}}'

echo -e "\ngetting membership_list, response:"
./client.sh --wait-complete membership_list_get 1

echo -e "\ngetting reverse membership list POD, response:"
./client.sh reverse_membership_list_pod_get 1

echo -e "\ngetting proof of membership, response:"
./client.sh --wait-complete user_get 1 alice

echo -e "\ndel from membership_list, response:"
./client.sh --wait-complete membership_list_update 1 '{"del":{"group":"blue","user":"alice"}}'

echo -e "\ngetting membership_list, response:"
./client.sh --wait-complete membership_list_get 1

echo -e "\ngetting the state from the Synchronizer server"
curl -X GET http://0.0.0.0:8001/ad_state/0000000000000000000000000000000000000000000000000000000000000001

echo -e ""
