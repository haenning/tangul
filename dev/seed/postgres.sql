-- Sample data for local development (runs once on first container start).

CREATE SCHEMA app;

CREATE TABLE app.users (
    id         serial PRIMARY KEY,
    email      text NOT NULL UNIQUE,
    name       text,
    active     boolean NOT NULL DEFAULT true,
    created_at timestamptz NOT NULL DEFAULT now(),
    meta       jsonb
);

INSERT INTO app.users (email, name, active, meta)
SELECT 'user' || g || '@example.com',
       CASE WHEN g % 7 = 0 THEN NULL ELSE 'User ' || g END,
       g % 5 <> 0,
       jsonb_build_object('plan', CASE WHEN g % 3 = 0 THEN 'pro' ELSE 'free' END, 'n', g)
FROM generate_series(1, 100000) g;

CREATE TABLE app.orders (
    id       bigserial PRIMARY KEY,
    user_id  int NOT NULL REFERENCES app.users (id),
    total    numeric(10, 2) NOT NULL,
    status   text NOT NULL,
    note     text,
    ordered  date NOT NULL
);

INSERT INTO app.orders (user_id, total, status, note, ordered)
SELECT (g % 100000) + 1,
       round((random() * 500)::numeric, 2),
       (ARRAY['open', 'paid', 'shipped', 'cancelled'])[1 + g % 4],
       CASE WHEN g % 11 = 0 THEN 'multi' || chr(10) || 'line note' END,
       date '2024-01-01' + (g % 700)
FROM generate_series(1, 300000) g;

CREATE VIEW app.revenue_by_status AS
SELECT status, count(*) AS orders, sum(total) AS revenue
FROM app.orders
GROUP BY status;

CREATE TABLE public.types_showcase (
    id        int PRIMARY KEY,
    u         uuid,
    arr       int[],
    range     int4range,
    bytes     bytea,
    ts        timestamp,
    long_text text
);

INSERT INTO public.types_showcase VALUES
    (1, gen_random_uuid(), '{1,2,3}', '[1,10)', '\xdeadbeef', now(), repeat('long ', 200)),
    (2, NULL, NULL, NULL, NULL, NULL, 'Ümlaut ✓ emoji 🚀');
