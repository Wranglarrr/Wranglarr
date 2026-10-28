#!/bin/bash
# Runs once on first cluster init (docker-entrypoint-initdb.d); also safe to
# re-run by hand after enabling another app:
#   docker compose exec postgres /docker-entrypoint-initdb.d/01-create-arr-dbs.sh
#
# $ARR_APPS is the COMPOSE_PROFILES list (comma or space separated).  For each
# arr it creates <app> (its main database) and <app>-log; bazarr gets a single
# "bazarr" database (wiki.bazarr.media), seerr a "seerr" one (docs.seerr.dev).  Unknown names are ignored.
set -euo pipefail

: "${POSTGRES_USER:?}"
: "${ARR_APPS:=radarr sonarr lidarr}"

create_db() {
  local db=$1
  if [[ $(psql -v ON_ERROR_STOP=1 -tAq -U "$POSTGRES_USER" -d postgres \
          -c "SELECT 1 FROM pg_database WHERE datname = '${db}'") == "1" ]]; then
    echo "initdb: database ${db} already exists"
  else
    echo "initdb: creating database ${db}"
    psql -v ON_ERROR_STOP=1 -U "$POSTGRES_USER" -d postgres \
      -c "CREATE DATABASE \"${db}\" OWNER \"${POSTGRES_USER}\""
  fi
}

for app in ${ARR_APPS//,/ }; do
  case "$app" in
    radarr|sonarr|lidarr|readarr|prowlarr|whisparr) create_db "${app}"; create_db "${app}-log" ;;
    bazarr) create_db "bazarr" ;;
    seerr) create_db "seerr" ;;
    *) ;;
  esac
done
