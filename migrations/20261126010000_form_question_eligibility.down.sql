DO $$
BEGIN
    IF EXISTS (SELECT FROM catalog.form_types WHERE NOT default_question_eligible)
       OR EXISTS (SELECT FROM lexicon_next.draft_revisions WHERE document ? 'form_question_eligibility')
       OR EXISTS (SELECT FROM lexicon_next.publications WHERE (snapshot->'draft') ? 'form_question_eligibility') THEN
        RAISE EXCEPTION 'Form question settings exist; rollback would lose data';
    END IF;
END $$;
ALTER TABLE catalog.form_types DROP COLUMN default_question_eligible;
