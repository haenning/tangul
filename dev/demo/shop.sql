-- `make demo`: a small shop for the README animation, in its own schema so the dev
-- seed stays as the tests expect it. Two years of orders that grow, peak before
-- Christmas, and grow at a different pace per country. Rebuilt on every run, the
-- same each time.

SET client_min_messages = warning;
DROP SCHEMA IF EXISTS demo CASCADE;
CREATE SCHEMA demo;
SELECT setseed(0.42) \g /dev/null

CREATE TABLE demo.countries (
    code   text PRIMARY KEY,
    name   text NOT NULL,
    -- Orders a day at the start, and how much that grows over the two years.
    base   int NOT NULL,
    growth numeric NOT NULL
);
INSERT INTO demo.countries VALUES
    ('US', 'United States',  70, 0.5),
    ('DE', 'Germany',        45, 1.1),
    ('GB', 'United Kingdom', 32, 0.3),
    ('FR', 'France',         20, 0.9),
    ('JP', 'Japan',           6, 4.0);

CREATE TABLE demo.categories (
    name  text PRIMARY KEY,
    share numeric NOT NULL, -- of the orders
    price numeric NOT NULL  -- an average order
);
INSERT INTO demo.categories VALUES
    ('Electronics', 0.22, 240),
    ('Home',        0.24,  95),
    ('Fashion',     0.28,  70),
    ('Sports',      0.14, 110),
    ('Books',       0.12,  28);

CREATE TABLE demo.orders (
    id       bigserial PRIMARY KEY,
    country  text NOT NULL REFERENCES demo.countries (code),
    category text NOT NULL REFERENCES demo.categories (name),
    total    numeric(10, 2) NOT NULL,
    ordered  date NOT NULL
);

WITH days AS (
    SELECT d::date AS day,
           (d::date - date '2024-01-01') / 730.0 AS t,
           -- Quiet in spring, busy from October, the peak in December.
           1 + 0.45 * greatest(0, extract(month FROM d) - 9) / 3
             - 0.12 * (extract(month FROM d) BETWEEN 2 AND 4)::int AS season
    FROM generate_series(date '2024-01-01', date '2025-12-31', interval '1 day') d
),
per_day AS (
    SELECT c.code, d.day,
           round(c.base * (1 + c.growth * d.t) * d.season * (0.8 + random() * 0.4))::int AS n
    FROM demo.countries c CROSS JOIN days d
),
picked AS (
    SELECT p.code, p.day, random() AS r
    FROM per_day p, generate_series(1, p.n)
),
bounds AS (
    SELECT name, price,
           sum(share) OVER (ORDER BY name) - share AS lo,
           sum(share) OVER (ORDER BY name) AS hi
    FROM demo.categories
)
INSERT INTO demo.orders (country, category, total, ordered)
SELECT p.code, b.name, round((b.price * (0.3 + random() * 1.4))::numeric, 2), p.day
FROM picked p
JOIN bounds b ON p.r >= b.lo AND p.r < b.hi;

ANALYZE demo.orders;

-- The assistant's SELECT-only user, as the database form would set it up.
DO $$ BEGIN
    IF NOT EXISTS (SELECT FROM pg_roles WHERE rolname = 'ai_reader') THEN
        CREATE ROLE ai_reader LOGIN;
    END IF;
END $$;
GRANT USAGE ON SCHEMA app, demo TO ai_reader;
GRANT SELECT ON ALL TABLES IN SCHEMA app, demo TO ai_reader;
