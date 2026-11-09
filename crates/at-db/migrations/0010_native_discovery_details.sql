-- Migration v10: how a native entry itself fared, and the folder a link resolves to.
--
-- Skill manager tools install skills as symbolic links into one central library, so the entry
-- an agent loads is a link and the content lives elsewhere. `native_items.path` keeps naming
-- that entry (it is what the agent reads and what adoption replaces); `link_target` records the
-- folder the link resolved to after the target was checked against the allowed roots, and the
-- Skills screen groups the links several agents share by it. `discovery_state` carries what the
-- last scan observed about the entry itself (`valid`, `invalid`, `broken_link`, `unreadable`),
-- so a refused or broken entry stays listed with its reason instead of being dropped from the
-- scan and then marked `missing` by the next refresh. `status` keeps its lifecycle meaning.
ALTER TABLE native_items ADD COLUMN link_target TEXT;
ALTER TABLE native_items ADD COLUMN discovery_state TEXT;
ALTER TABLE native_items ADD COLUMN discovery_note TEXT;
--
-- Downgrade: the columns are additive and nullable, so nothing is rewritten or dropped. A binary
-- that only knows v9 refuses to open a v10 index (the migration runner checks the recorded
-- versions and their hashes before writing), so an older build fails loudly instead of reading
-- or damaging a newer schema; restoring the previous binary needs an index rebuild from the
-- native folders, which is derived data.
