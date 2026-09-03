DO $$
BEGIN
    IF EXISTS (SELECT 1 FROM lexicon.v3_phrase_sense_component_usages)
       OR EXISTS (
           SELECT 1 FROM lexicon.nodes
           WHERE node_role = 'meanings.phrase_component_usage'
       ) THEN
        RAISE EXCEPTION 'cannot remove phrase sense component usages while draft data exists'
            USING ERRCODE = '0A000';
    END IF;
END
$$;

DROP TABLE lexicon.v3_phrase_sense_component_usages;
