-- Keep historical registrations intact. New saves record only active inherited accents.
ALTER TABLE speech.dubbing_dependencies
    ADD COLUMN accents TEXT[] NOT NULL DEFAULT ARRAY['gb','us']::TEXT[],
    ADD CONSTRAINT dubbing_dependencies_accents CHECK
        (cardinality(accents) BETWEEN 1 AND 2 AND accents <@ ARRAY['gb','us']::TEXT[]);
