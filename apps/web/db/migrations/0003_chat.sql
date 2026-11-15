ALTER TYPE usage_kind ADD VALUE 'chat';
--> statement-breakpoint
-- AI counters are locked before the shared admission function; all changes commit together.
CREATE FUNCTION xflow_admit_chat(p_user text, p_cost int, p_day int, p_month int, p_ai_day int, p_ai_month int)
RETURNS text LANGUAGE plpgsql AS $$
DECLARE
  kd text := 'ai:' || p_user || ':day';
  km text := 'ai:' || p_user || ':month';
  wd timestamptz := date_trunc('day', now() AT TIME ZONE 'UTC') AT TIME ZONE 'UTC';
  wm timestamptz := date_trunc('month', now() AT TIME ZONE 'UTC') AT TIME ZONE 'UTC';
  cd int; cm int; result text;
BEGIN
  IF p_cost < 0 THEN RAISE EXCEPTION 'negative reservation'; END IF;
  INSERT INTO rate_limits(key, window_start, count)
    SELECT k, 'epoch'::timestamptz, 0 FROM unnest(ARRAY[kd,km]) k ON CONFLICT DO NOTHING;
  PERFORM 1 FROM rate_limits WHERE key = ANY(ARRAY[kd,km]) ORDER BY key FOR UPDATE;
  SELECT CASE WHEN window_start = wd THEN count ELSE 0 END INTO cd FROM rate_limits WHERE key = kd;
  SELECT CASE WHEN window_start = wm THEN count ELSE 0 END INTO cm FROM rate_limits WHERE key = km;
  IF cd + p_cost > p_ai_day THEN RETURN 'ai_budget_day'; END IF;
  IF cm + p_cost > p_ai_month THEN RETURN 'ai_budget_month'; END IF;
  result := xflow_admit(p_user, 'chat', 20, 500, 1, 90, p_cost, p_day, p_month);
  IF result <> 'ok' THEN RETURN result; END IF;
  UPDATE rate_limits SET count = cd + p_cost, window_start = wd WHERE key = kd;
  UPDATE rate_limits SET count = cm + p_cost, window_start = wm WHERE key = km;
  RETURN 'ok';
END $$;
