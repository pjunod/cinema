-- One paced allowance per provider. Credits do not accumulate into bursts.
CREATE TABLE IF NOT EXISTS background_provider_budgets (
    provider TEXT PRIMARY KEY CHECK(provider IN ('tmdb','ani_list')),
    next_dispatch_ms INTEGER NOT NULL DEFAULT 0,
    interval_ms INTEGER NOT NULL CHECK(interval_ms BETWEEN 100 AND 60000)
) STRICT;
-- next statement
CREATE TRIGGER IF NOT EXISTS background_provider_budget_command
AFTER INSERT ON background_job_commands WHEN NEW.operation = 'provider_budget'
BEGIN
    INSERT INTO background_provider_budgets(provider, interval_ms)
    SELECT json_extract(NEW.request_json, '$.provider'),
        CASE json_extract(NEW.request_json, '$.provider') WHEN 'tmdb' THEN 100 ELSE 2100 END
    WHERE json_extract(NEW.result_json, '$.outcome') IN ('charged','observed')
    ON CONFLICT(provider) DO NOTHING;
    UPDATE background_provider_budgets SET
        next_dispatch_ms = CASE json_extract(NEW.result_json, '$.outcome')
            WHEN 'charged' THEN json_extract(NEW.request_json, '$.now_ms') + interval_ms
            ELSE MAX(next_dispatch_ms, json_extract(NEW.request_json, '$.now_ms') + json_extract(NEW.request_json, '$.action.cooldown_ms')) END,
        interval_ms = CASE json_extract(NEW.result_json, '$.outcome') WHEN 'observed'
            THEN MAX(CASE provider WHEN 'tmdb' THEN 100 ELSE 2100 END,
                COALESCE(json_extract(NEW.request_json, '$.action.interval_ms'), interval_ms))
            ELSE interval_ms END
    WHERE provider = json_extract(NEW.request_json, '$.provider')
        AND json_extract(NEW.result_json, '$.outcome') IN ('charged','observed');
    DELETE FROM background_job_commands WHERE id = NEW.id;
END;
