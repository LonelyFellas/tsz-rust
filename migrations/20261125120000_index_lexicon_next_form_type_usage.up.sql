CREATE INDEX draft_revision_form_type_usage_idx ON lexicon_next.draft_revisions
    USING gin (jsonb_path_query_array(document, '$.form_editor.pos[*].forms[*].form_type') jsonb_path_ops);

CREATE INDEX publication_form_type_usage_idx ON lexicon_next.publications
    USING gin (jsonb_path_query_array(snapshot, '$.draft.form_editor.pos[*].forms[*].form_type') jsonb_path_ops);
