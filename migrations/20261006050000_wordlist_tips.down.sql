LOCK TABLE wordlist_tips,wordlist_items,lexicon.entries IN ACCESS EXCLUSIVE MODE;
DO $$ BEGIN
    IF EXISTS(SELECT 1 FROM wordlist_tips) OR EXISTS(SELECT 1 FROM coin_operations WHERE source_type='wordlist_tip') THEN
        RAISE EXCEPTION 'cannot remove wordlist tip financial evidence';
    END IF;
END $$;
DROP TABLE wordlist_tips;
DROP FUNCTION preserve_wordlist_tip();
ALTER TABLE wordlist_items DROP COLUMN approved_archive_generation;
DROP TRIGGER wordlist_archive_generation ON lexicon.entries;
DROP FUNCTION advance_wordlist_archive_generation();
ALTER TABLE lexicon.entries DROP COLUMN wordlist_archive_generation;
