#!/bin/sh
set -e

RESOLVER=$(grep nameserver /etc/resolv.conf | awk '{print $2}' | head -1)
echo "nginx: using resolver $RESOLVER"

sed "s|DOCKER_RESOLVER|$RESOLVER|g" \
    /etc/nginx/default.conf.tmpl \
    > /etc/nginx/conf.d/default.conf

exec "$@"
