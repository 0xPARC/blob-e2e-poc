#!/usr/bin/env bash

set -e

BASE_URL="http://0.0.0.0:8000"

usage () {
	echo "Usage: $0 [--wait-complete] ARGS"
	echo "ARGS:"
	echo "    request_get REQ_ID"
	echo "    membership_list_get AD_ID"
	echo "    membership_list_create"
	echo "    membership_list_update AD_ID OP"
	echo "    user_get AD_ID USER"
}

CURL_OPTS="--silent"

wait_complete=false
if [[ "$1" == "--wait-complete" ]]; then
	wait_complete=true
	shift
fi

resp=""
case "$1" in
	request_get)
		req_id=$2
		resp=$(curl $CURL_OPTS -X GET "$BASE_URL/request/$req_id")
		;;
	membership_list_get)
		ad_id=$2
		resp=$(curl $CURL_OPTS -X GET "$BASE_URL/membership_list/$ad_id")
		wait_complete=false
		;;
	membership_list_create)
		resp=$(curl $CURL_OPTS -X POST "$BASE_URL/membership_list")
		;;
	membership_list_update)
		ad_id=$2
		op=$3
		resp=$(curl $CURL_OPTS --json "$op" "$BASE_URL/membership_list/$ad_id")
		;;
	user_get)
		ad_id=$2
		user=$3
		resp=$(curl $CURL_OPTS -X GET "$BASE_URL/user/$ad_id/$user")
		;;
	reverse_membership_list_pod_get)
		ad_id=$2
		resp=$(curl $CURL_OPTS -X GET "$BASE_URL/reverse_membership_list_pod/$ad_id")
		;;
	*)
		usage
		exit 1
esac

if $wait_complete; then
	echo "Waiting on $resp ..." >&2 # Log to stderr
	req_id=$(echo "$resp" | jq --raw-output .req_id)
	while true; do
		resp=$(curl $CURL_OPTS -X GET "$BASE_URL/request/$req_id")
		echo "$(date) -" "$resp" >&2 # Log to stderr
		state_case=$(echo "$resp" | jq --raw-output "keys[0]")
		complete_data=$(echo "$resp" | jq --compact-output ".${state_case}.Complete?")
		if ! ([[ "$complete_data" == "null" ]] || [[ "$complete_data" == "" ]]); then
			echo "$complete_data"
			exit 0
		fi
		sleep 5
	done
else
	echo $resp
fi
