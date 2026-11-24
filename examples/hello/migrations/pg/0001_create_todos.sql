-- Migration: create_todos
CREATE TABLE todos (
    id bigserial PRIMARY KEY,
    title text NOT NULL,
    done boolean NOT NULL DEFAULT false,
    tags jsonb NOT NULL DEFAULT '[]',
    created_at timestamptz NOT NULL DEFAULT now()
);
