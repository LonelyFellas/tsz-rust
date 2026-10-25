-- Rollback removes synthesis binding metadata. Deployments must stop new generated-audio
-- writes and remove/restore affected draft/publication fields before running this down.
DROP TABLE lexicon.audio_generations;
