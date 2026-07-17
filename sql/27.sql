-- Revises: 26
-- Reason: Drop the runtime "Public AI" toggle left behind by the removed
--         "Ask the AI" assistant.
--
-- The assistant's token-spend gate was account-gated by default and could be
-- opened to anonymous visitors at runtime; that switch was persisted in the
-- `storage` KV table under `ai_public` rather than in `config.json`, so removing
-- the feature's code leaves the row behind with nothing to read it.
--
-- Unlike the table drops in 23-26 this is a row delete, because `storage` is a
-- general-purpose key/value table the site still uses — only this one key goes.
--
-- `DELETE` is a no-op on the databases that never had the toggle flipped (the
-- row is only written when an admin actually changed it), so this converges
-- every state the same way.

DELETE FROM storage WHERE name = 'ai_public';
