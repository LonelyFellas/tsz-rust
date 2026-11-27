-- PostgreSQL rejects this rollback if any rows now share a complete triple.
CREATE UNIQUE INDEX lexicon_v3_pronunciations_complete_triple_key
    ON lexicon.v3_pronunciations (
        form_variant_id,
        normalized_dict_phonetic,
        normalized_actual_pron,
        style,
        normalization_version
    )
    WHERE style IS NOT NULL
      AND normalized_dict_phonetic <> ''
      AND normalized_actual_pron <> '';
