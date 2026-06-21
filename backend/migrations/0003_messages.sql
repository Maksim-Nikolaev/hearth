CREATE TABLE messages (
    id          uuid PRIMARY KEY DEFAULT uuidv7(),
    room        text NOT NULL,
    sender_user uuid NOT NULL REFERENCES users(id),
    body        text NOT NULL,
    created_at  timestamptz NOT NULL DEFAULT now()
);

CREATE INDEX messages_room_created_idx ON messages (room, created_at);
