-- Payload-lookup index for the managed write path: unpublished_payloads and
-- payload_publication_stats probe byte_extents by (volume_id, payload_id)
-- once per payload; without it every probe is a full table scan.
CREATE INDEX IF NOT EXISTS byte_extents_payload
    ON byte_extents(volume_id, payload_id)
    WHERE payload_id IS NOT NULL;
