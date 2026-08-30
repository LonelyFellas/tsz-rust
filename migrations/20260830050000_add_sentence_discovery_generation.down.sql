DROP TRIGGER lexicon_surface_sources_discovery_generation_trigger
    ON lexicon.surface_sources;
DROP FUNCTION lexicon.bump_sentence_discovery_generation();
DROP TABLE lexicon.sentence_discovery_generation;
