-- After pgloader: moves every id counter past the highest id that was copied in, so the
-- app's next new row doesn't collide with an old one ("duplicate key value violates
-- unique constraint ..._pkey"). Safe to run more than once.
--
--   docker compose exec -T postgres psql -U "$POSTGRES_USER" -d radarr < postgres/migrate/reset-sequences.sql
DO $$
DECLARE r record;
BEGIN
  FOR r IN
    SELECT s.oid::regclass AS seq, t.oid::regclass AS tbl, a.attname AS col
      FROM pg_class s
      JOIN pg_depend d ON d.objid = s.oid AND d.classid = 'pg_class'::regclass AND d.deptype IN ('a', 'i')
      JOIN pg_class t ON t.oid = d.refobjid
      JOIN pg_attribute a ON a.attrelid = t.oid AND a.attnum = d.refobjsubid
     WHERE s.relkind = 'S'
  LOOP
    EXECUTE format('SELECT setval(%L, COALESCE((SELECT MAX(%I) FROM %s), 0) + 1, false)', r.seq, r.col, r.tbl);
  END LOOP;
END $$;
