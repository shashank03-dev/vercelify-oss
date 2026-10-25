-- Webhooks per git source (design §11): the secret the provider signs
-- events with (sealed).
ALTER TABLE git_sources ADD COLUMN webhook_secret BLOB;
