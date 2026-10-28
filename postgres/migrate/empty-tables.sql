-- Empties every table in the database you run it against, before pgloader copies an old
-- SQLite database in (wiki/Importing-Existing-Apps.md). The app must be stopped.
-- A new install's tables hold only the defaults it started with; the old database has all
-- of its own rows, so emptying everything first avoids duplicates, whatever the app.
-- One exception: Seerr's "migrations" table lists the Postgres migrations it ran, which
-- differ from its SQLite ones, so it stays (seerr.load skips the old one).
--
--   docker compose exec -T postgres psql -U "$POSTGRES_USER" -d radarr < postgres/migrate/empty-tables.sql
DO $$
DECLARE t text;
BEGIN
  SELECT string_agg(format('%I.%I', schemaname, tablename), ', ') INTO t
    FROM pg_tables WHERE schemaname = 'public' AND tablename <> 'migrations';
  IF t IS NOT NULL THEN
    EXECUTE 'TRUNCATE ' || t;
  END IF;
END $$;
