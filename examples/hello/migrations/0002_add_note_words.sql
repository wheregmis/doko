-- Migration number: 0002 	 add_note_words
-- Filled in by the CountWords job after each insert.
ALTER TABLE notes ADD COLUMN words INTEGER;
