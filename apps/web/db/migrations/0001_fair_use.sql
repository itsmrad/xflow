-- Admission for the hosted proxy in one round trip, under row locks:
-- concurrency slot, per-minute and per-day request counts, and a daily and monthly
-- provider-spend budget in micro-dollars. Nothing is counted unless the request is admitted.
CREATE OR REPLACE FUNCTION xflow_admit(
  p_user text, p_kind text,
  p_per_minute int, p_per_day int, p_concurrent int, p_lease_seconds int,
  p_cost int, p_day_budget int, p_month_budget int
) RETURNS text LANGUAGE plpgsql AS $$
DECLARE
  k_min  text := p_kind || ':' || p_user || ':min';
  k_day  text := p_kind || ':' || p_user || ':day';
  k_slot text := p_kind || ':' || p_user || ':inflight';
  k_sday text := 'spend:' || p_user || ':day';
  k_smon text := 'spend:' || p_user || ':month';
  w_min  timestamptz := date_trunc('minute', now());
  w_day  timestamptz := date_trunc('day', now() AT TIME ZONE 'UTC') AT TIME ZONE 'UTC';
  w_mon  timestamptz := date_trunc('month', now() AT TIME ZONE 'UTC') AT TIME ZONE 'UTC';
  c_min int; c_day int; c_slot int; c_sday int; c_smon int;
  slot_start timestamptz;
BEGIN
  INSERT INTO rate_limits (key, window_start, count)
  SELECT k, 'epoch'::timestamptz, 0 FROM unnest(ARRAY[k_min, k_day, k_slot, k_sday, k_smon]) AS k
  ON CONFLICT (key) DO NOTHING;
  -- One lock order for every caller, so concurrent admits can't deadlock.
  PERFORM 1 FROM rate_limits WHERE key = ANY (ARRAY[k_min, k_day, k_slot, k_sday, k_smon]) ORDER BY key FOR UPDATE;

  SELECT CASE WHEN window_start = w_min THEN count ELSE 0 END INTO c_min FROM rate_limits WHERE key = k_min;
  SELECT CASE WHEN window_start = w_day THEN count ELSE 0 END INTO c_day FROM rate_limits WHERE key = k_day;
  SELECT CASE WHEN window_start = w_day THEN count ELSE 0 END INTO c_sday FROM rate_limits WHERE key = k_sday;
  SELECT CASE WHEN window_start = w_mon THEN count ELSE 0 END INTO c_smon FROM rate_limits WHERE key = k_smon;
  SELECT count, window_start INTO c_slot, slot_start FROM rate_limits WHERE key = k_slot;
  -- The lease runs from when the slot count last left zero, so a slot leaked by a killed
  -- function frees itself even while the user keeps making requests.
  IF c_slot > 0 AND slot_start < now() - make_interval(secs => p_lease_seconds) THEN
    c_slot := 0;
  END IF;

  IF c_slot >= p_concurrent THEN RETURN 'concurrency'; END IF;
  IF c_min >= p_per_minute THEN RETURN 'minute'; END IF;
  IF c_day >= p_per_day THEN RETURN 'day'; END IF;
  IF c_sday + p_cost > p_day_budget THEN RETURN 'budget_day'; END IF;
  IF c_smon + p_cost > p_month_budget THEN RETURN 'budget_month'; END IF;

  UPDATE rate_limits SET count = c_min + 1, window_start = w_min WHERE key = k_min;
  UPDATE rate_limits SET count = c_day + 1, window_start = w_day WHERE key = k_day;
  UPDATE rate_limits SET count = c_sday + p_cost, window_start = w_day WHERE key = k_sday;
  UPDATE rate_limits SET count = c_smon + p_cost, window_start = w_mon WHERE key = k_smon;
  UPDATE rate_limits
     SET count = c_slot + 1, window_start = CASE WHEN c_slot = 0 THEN now() ELSE window_start END
   WHERE key = k_slot;
  RETURN 'ok';
END $$;
--> statement-breakpoint
-- Settles a reservation once the real cost is known (delta may be negative).
-- A window that rolled over meanwhile is left alone.
CREATE OR REPLACE FUNCTION xflow_settle(p_user text, p_delta int) RETURNS void LANGUAGE sql AS $$
  UPDATE rate_limits SET count = greatest(count + p_delta, 0)
   WHERE (key = 'spend:' || p_user || ':day'
          AND window_start = date_trunc('day', now() AT TIME ZONE 'UTC') AT TIME ZONE 'UTC')
      OR (key = 'spend:' || p_user || ':month'
          AND window_start = date_trunc('month', now() AT TIME ZONE 'UTC') AT TIME ZONE 'UTC');
$$;
