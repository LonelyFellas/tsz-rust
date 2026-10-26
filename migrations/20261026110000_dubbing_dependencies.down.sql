DO $$
BEGIN
    IF EXISTS (SELECT 1 FROM speech.dubbing_dependencies)
        OR EXISTS (
            SELECT 1 FROM lexicon.entry_editor_projection
            WHERE jsonb_path_exists(jsonb_build_array(forms,meanings), '$.**.dubbing')
               OR jsonb_path_exists(meanings, '$.**.reading_reference')
               OR jsonb_path_exists(meanings, '$.**.reading_links[*]')
               OR jsonb_path_exists(jsonb_build_array(forms,meanings), '$.**.annotations[*] ? (@.type == "say_as" || @.type == "sub")')
        ) OR EXISTS (
            SELECT 1 FROM lexicon.entry_publications
            WHERE jsonb_path_exists(snapshot, '$.**.dubbing')
               OR jsonb_path_exists(snapshot, '$.**.reading_reference')
               OR jsonb_path_exists(snapshot, '$.**.reading_links[*]')
               OR jsonb_path_exists(snapshot, '$.**.annotations[*] ? (@.type == "say_as" || @.type == "sub")')
        ) OR EXISTS (
            SELECT 1 FROM lexicon.shared_sentences
            WHERE jsonb_path_exists(content, '$.**.dubbing')
               OR jsonb_path_exists(content, '$.**.reading_links[*]')
               OR jsonb_path_exists(content, '$.**.annotations[*] ? (@.type == "say_as" || @.type == "sub")')
        ) OR EXISTS (
            SELECT 1 FROM lexicon.shared_sentence_publications
            WHERE jsonb_path_exists(snapshot, '$.**.dubbing')
               OR jsonb_path_exists(snapshot, '$.**.reading_links[*]')
               OR jsonb_path_exists(snapshot, '$.**.annotations[*] ? (@.type == "say_as" || @.type == "sub")')
        ) THEN
        RAISE EXCEPTION 'Cannot roll back saved dubbing modes, reading references or special readings';
    END IF;
END $$;
DROP TABLE speech.dubbing_dependencies;
