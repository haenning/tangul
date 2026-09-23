-- Sample data for local development (runs once on first container start).
SET NAMES utf8mb4;

USE app;

CREATE TABLE digits (d INT PRIMARY KEY);
INSERT INTO digits VALUES (0), (1), (2), (3), (4), (5), (6), (7), (8), (9);

CREATE TABLE users (
    id         INT PRIMARY KEY AUTO_INCREMENT,
    email      VARCHAR(255) NOT NULL UNIQUE,
    name       VARCHAR(255),
    active     BOOLEAN NOT NULL DEFAULT TRUE,
    created_at DATETIME NOT NULL DEFAULT CURRENT_TIMESTAMP,
    meta       JSON
);

-- 100k rows from a digits cross join (portable, no recursive CTE limits).
INSERT INTO users (email, name, active, meta)
SELECT CONCAT('user', n, '@example.com'),
       IF(n % 7 = 0, NULL, CONCAT('User ', n)),
       n % 5 <> 0,
       JSON_OBJECT('plan', IF(n % 3 = 0, 'pro', 'free'), 'n', n)
FROM (
    SELECT a.d + b.d * 10 + c.d * 100 + e.d * 1000 + f.d * 10000 + 1 AS n
    FROM digits a, digits b, digits c, digits e, digits f
) t;

CREATE TABLE orders (
    id      BIGINT PRIMARY KEY AUTO_INCREMENT,
    user_id INT NOT NULL,
    total   DECIMAL(10, 2) NOT NULL,
    status  VARCHAR(20) NOT NULL,
    ordered DATE NOT NULL,
    FOREIGN KEY (user_id) REFERENCES users (id)
);

INSERT INTO orders (user_id, total, status, ordered)
SELECT (n % 100000) + 1,
       ROUND(RAND() * 500, 2),
       ELT(1 + n % 4, 'open', 'paid', 'shipped', 'cancelled'),
       DATE_ADD('2024-01-01', INTERVAL n % 700 DAY)
FROM (
    SELECT a.d + b.d * 10 + c.d * 100 + e.d * 1000 + f.d * 10000 AS n
    FROM digits a, digits b, digits c, digits e, digits f
) t;

CREATE VIEW revenue_by_status AS
SELECT status, COUNT(*) AS orders, SUM(total) AS revenue
FROM orders
GROUP BY status;

CREATE TABLE types_showcase (
    id        INT PRIMARY KEY,
    bytes     BLOB,
    ts        TIMESTAMP NULL,
    long_text TEXT
);

INSERT INTO types_showcase VALUES
    (1, UNHEX('DEADBEEF'), NOW(), REPEAT('long ', 200)),
    (2, NULL, NULL, 'Ümlaut ✓ emoji 🚀');
