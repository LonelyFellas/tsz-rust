ALTER TABLE learning_cycle_units
 ADD COLUMN custom_item_id uuid,
 ALTER COLUMN entry_id DROP NOT NULL,
 ALTER COLUMN entry_archive_generation DROP NOT NULL,
 ALTER COLUMN publication_id DROP NOT NULL,
 ALTER COLUMN pos_id DROP NOT NULL,
 ALTER COLUMN sense_id DROP NOT NULL,
 ALTER COLUMN definition_id DROP NOT NULL;
ALTER TABLE learning_cycle_units ADD CONSTRAINT learning_cycle_units_source_identity CHECK (
 (custom_item_id IS NULL AND entry_id IS NOT NULL AND entry_archive_generation IS NOT NULL AND publication_id IS NOT NULL AND pos_id IS NOT NULL AND sense_id IS NOT NULL AND definition_id IS NOT NULL) OR
 (custom_item_id IS NOT NULL AND entry_id IS NULL AND entry_archive_generation IS NULL AND publication_id IS NULL AND pos_id IS NULL AND sense_id IS NULL AND definition_id IS NULL AND cardinality(form_ids)=0)
);
ALTER TABLE learning_questions
 ADD COLUMN custom_item_id uuid,
 ALTER COLUMN entry_id DROP NOT NULL,
 ALTER COLUMN entry_archive_generation DROP NOT NULL,
 ALTER COLUMN publication_id DROP NOT NULL,
 ALTER COLUMN pos_id DROP NOT NULL,
 ALTER COLUMN sense_id DROP NOT NULL,
 ALTER COLUMN definition_id DROP NOT NULL;
ALTER TABLE learning_questions ADD CONSTRAINT learning_questions_source_identity CHECK (
 (custom_item_id IS NULL AND entry_id IS NOT NULL AND entry_archive_generation IS NOT NULL AND publication_id IS NOT NULL AND pos_id IS NOT NULL AND sense_id IS NOT NULL AND definition_id IS NOT NULL) OR
 (custom_item_id IS NOT NULL AND entry_id IS NULL AND entry_archive_generation IS NULL AND publication_id IS NULL AND pos_id IS NULL AND sense_id IS NULL AND definition_id IS NULL AND cardinality(form_ids)=0)
);
