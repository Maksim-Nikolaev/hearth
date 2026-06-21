CREATE TABLE users (
    id          uuid PRIMARY KEY DEFAULT uuidv7(),
    username    text NOT NULL UNIQUE,
    password_hash text NOT NULL,
    role        text NOT NULL DEFAULT 'USER' CHECK (role IN ('USER', 'ADMIN')),
    created_at  timestamptz NOT NULL DEFAULT now()
);
