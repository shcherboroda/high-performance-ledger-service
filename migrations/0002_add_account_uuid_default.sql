CREATE EXTENSION IF NOT EXISTS pgcrypto;

ALTER TABLE accounts
    ALTER COLUMN id SET DEFAULT gen_random_uuid();
