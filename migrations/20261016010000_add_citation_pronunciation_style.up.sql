-- Keep normal as the existing connected-speech reading; citation is a distinct reading.
ALTER TABLE lexicon.v3_pronunciations
    DROP CONSTRAINT lexicon_v3_pronunciations_style_check,
    ADD CONSTRAINT lexicon_v3_pronunciations_style_check
        CHECK (style IN ('normal', 'citation', 'strong', 'weak'));
