-- Claude can now edit and remove things too, and manage clients: creates
-- and updates apply at once (recorded, revertible); removals wait for you.
ALTER TABLE proposals DROP CONSTRAINT proposals_kind_check;
ALTER TABLE proposals ADD CONSTRAINT proposals_kind_check CHECK (kind IN (
  'doc_edit', 'doc_create', 'doc_delete',
  'rule', 'rule_edit', 'rule_retire',
  'decision', 'decision_edit', 'decision_retire',
  'artefact_link', 'artefact_unlink',
  'library_put', 'library_pin', 'library_delete',
  'client_create', 'client_update', 'client_delete'
));
-- The hands-off switch is gone: each kind's policy says whether it waits.
DELETE FROM settings WHERE key = 'hands_off';
