ALTER TABLE admins ADD COLUMN lexicon_languages JSONB
    CONSTRAINT admins_lexicon_languages_check CHECK (
        lexicon_languages IS NULL OR COALESCE(
            jsonb_typeof(lexicon_languages)='object'
            AND lexicon_languages->>'target_language'='en'
            AND lexicon_languages->>'target_locale' IN ('en-GB','en-US')
            AND lexicon_languages->>'medium_locale' IN ('zh-Hans','zh-Hant')
            AND lexicon_languages - ARRAY['target_language','target_locale','medium_locale'] = '{}'::jsonb,
            false
        )
    );
