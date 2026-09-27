-- Sample data for local development (runs once on first container start).
SET NAMES utf8mb4;

USE app;

CREATE TABLE digits (d INT PRIMARY KEY);
INSERT INTO digits VALUES (0), (1), (2), (3), (4), (5), (6), (7), (8), (9);

CREATE TABLE users (
    id         INT PRIMARY KEY AUTO_INCREMENT,
    email      VARCHAR(255) NOT NULL UNIQUE COMMENT 'Login; unique',
    name       VARCHAR(255),
    active     BOOLEAN NOT NULL DEFAULT TRUE COMMENT 'False once the account is closed',
    created_at DATETIME NOT NULL DEFAULT CURRENT_TIMESTAMP,
    meta       JSON COMMENT 'Settings as JSON: plan (free or pro) and n'
) COMMENT = 'Registered customers';

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
    total   DECIMAL(10, 2) NOT NULL CHECK (total >= 0),
    status  ENUM('open', 'paid', 'shipped', 'cancelled') NOT NULL
              COMMENT 'Where an order is: open until paid, then shipped',
    ordered DATE NOT NULL COMMENT 'Day the order was placed',
    FOREIGN KEY (user_id) REFERENCES users (id),
    INDEX orders_user_ordered (user_id, ordered)
) COMMENT = 'Customer orders; totals in EUR';

INSERT INTO orders (user_id, total, status, ordered)
SELECT (n % 100000) + 1,
       ROUND(RAND() * 500, 2),
       ELT(1 + n % 4, 'open', 'paid', 'shipped', 'cancelled'),
       DATE_ADD('2024-01-01', INTERVAL n % 700 DAY)
FROM (
    SELECT a.d + b.d * 10 + c.d * 100 + e.d * 1000 + f.d * 10000 AS n
    FROM digits a, digits b, digits c, digits e, digits f
) t;

-- More tables referencing users, so its "Show related" menu has more than three
-- entries (the rest go under "Show all").
CREATE TABLE addresses (
    id      INT PRIMARY KEY AUTO_INCREMENT,
    user_id INT NOT NULL,
    city    VARCHAR(50) NOT NULL,
    FOREIGN KEY (user_id) REFERENCES users (id)
);
INSERT INTO addresses (user_id, city)
SELECT n + 1, ELT(1 + n % 3, 'Berlin', 'Hamburg', 'Munich')
FROM (SELECT a.d + b.d * 10 + c.d * 100 AS n FROM digits a, digits b, digits c) t;

CREATE TABLE sessions (
    id      BIGINT PRIMARY KEY AUTO_INCREMENT,
    user_id INT NOT NULL,
    started DATETIME NOT NULL DEFAULT CURRENT_TIMESTAMP,
    FOREIGN KEY (user_id) REFERENCES users (id)
);
INSERT INTO sessions (user_id)
SELECT (n % 1000) + 1
FROM (SELECT a.d + b.d * 10 + c.d * 100 + e.d * 1000 AS n
      FROM digits a, digits b, digits c, digits e) t;

CREATE TABLE tickets (
    id      INT PRIMARY KEY AUTO_INCREMENT,
    user_id INT NOT NULL,
    subject VARCHAR(100) NOT NULL,
    FOREIGN KEY (user_id) REFERENCES users (id)
);
INSERT INTO tickets (user_id, subject)
SELECT (n % 500) + 1, CONCAT('Ticket ', n)
FROM (SELECT a.d + b.d * 10 + c.d * 100 AS n FROM digits a, digits b, digits c) t;

-- No primary key: UPDATE / DELETE from the grid match rows by all columns.
CREATE TABLE audit_log (
    user_id INT,
    action  VARCHAR(20) NOT NULL,
    at      DATETIME NOT NULL DEFAULT CURRENT_TIMESTAMP,
    FOREIGN KEY (user_id) REFERENCES users (id)
);
INSERT INTO audit_log (user_id, action)
SELECT (n % 100) + 1, ELT(1 + n % 2, 'login', 'logout')
FROM (SELECT a.d + b.d * 10 AS n FROM digits a, digits b) t;

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

-- A function with a comment, for the catalog (READS SQL DATA: allowed with binary
-- logging on, MySQL 8's default).
CREATE FUNCTION order_count(uid INT) RETURNS BIGINT READS SQL DATA
COMMENT 'Number of orders of a user'
RETURN (SELECT COUNT(*) FROM orders WHERE user_id = uid);

-- Row estimates come from statistics.
ANALYZE TABLE users, orders, addresses, sessions, tickets, audit_log;
