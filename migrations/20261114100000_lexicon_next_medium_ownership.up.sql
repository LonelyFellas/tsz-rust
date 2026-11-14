-- Ownership is independent of translated content, so unfinished drafts remain discoverable.
ALTER TABLE lexicon_next.entries ADD COLUMN medium_locale text NOT NULL DEFAULT 'zh-Hans'
    CHECK (medium_locale IN ('zh-Hans', 'zh-Hant'));
-- Approved legacy rule: only Traditional content -> Traditional; empty/mixed -> Simplified.
UPDATE lexicon_next.entries e SET medium_locale='zh-Hant'
WHERE EXISTS (
    SELECT 1 FROM lexicon_next.nodes n
    LEFT JOIN lexicon_next.definitions d ON d.id=n.id
    LEFT JOIN lexicon_next.equivalents q ON q.id=n.id
    WHERE n.entry_id=e.id AND COALESCE(d.locale,q.locale)='zh-Hant'
) AND NOT EXISTS (
    SELECT 1 FROM lexicon_next.nodes n
    LEFT JOIN lexicon_next.definitions d ON d.id=n.id
    LEFT JOIN lexicon_next.equivalents q ON q.id=n.id
    WHERE n.entry_id=e.id AND COALESCE(d.locale,q.locale)='zh-Hans'
);
CREATE INDEX lexicon_next_entries_language_medium ON lexicon_next.entries(target_language,medium_locale);
