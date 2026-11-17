-- Migration 0004: the owner's credentials live in the database (M5.1).
--
-- Single user, so one row (`id = 1`, created here). `password_phc` is the
-- argon2id PHC string; NULL means "not set up yet": the app then asks for
-- the one-time setup code it logs at start (design §9.1). The connector
-- token is stored only as its SHA-256 (hex); NULL means "no token", and the
-- connector routes refuse every request (§9.2). Sessions keep referring to
-- the password by the SHA-256 of `password_phc` (`auth_session`), so a new
-- password still ends the sessions of the old one.
CREATE TABLE auth_owner (
  id INTEGER PRIMARY KEY CHECK (id = 1),
  password_phc TEXT,
  password_set_at TEXT,
  connector_token_sha256 TEXT CHECK (length(connector_token_sha256) = 64),
  connector_token_created_at TEXT
) STRICT;
INSERT INTO auth_owner (id) VALUES (1);
