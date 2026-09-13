-- The annotation itself identifies the sense; there is no separate collection.
ALTER TABLE lexicon.shared_sentence_annotations
  ADD COLUMN target_sense_id UUID,
  ADD COLUMN target_ref JSONB,
  ADD CONSTRAINT shared_sentence_target_sense_fkey
    FOREIGN KEY (target_sense_id,target_entry_id) REFERENCES lexicon.nodes(id,entry_id),
  ADD CONSTRAINT shared_sentence_target_ref_check CHECK (
    (target_sense_id IS NULL AND target_ref IS NULL)
    OR (target_entry_id IS NOT NULL AND target_sense_id IS NOT NULL AND target_ref IS NOT NULL
      AND jsonb_typeof(target_ref)='object'
      AND target_ref ?& ARRAY['state','target_entry_id','target_sense_id']
      AND target_ref->>'state' IS NOT NULL
      AND target_ref->>'target_entry_id' IS NOT NULL
      AND target_ref->>'target_sense_id' IS NOT NULL
      AND target_ref->>'state'='linked'
      AND target_ref->>'target_entry_id'=target_entry_id::text
      AND target_ref->>'target_sense_id'=target_sense_id::text)
  );
CREATE INDEX shared_sentence_annotation_sense_idx
  ON lexicon.shared_sentence_annotations(target_entry_id,target_sense_id,sentence_id)
  WHERE target_sense_id IS NOT NULL;
