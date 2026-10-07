-- Payload packing: several small journal payloads publish inside ONE
-- provider object. `member_offset` is the byte offset of this payload's
-- encoded object inside the provider object; 0 rows are unpacked objects
-- (or the first member of a pack).
ALTER TABLE payload_remote_objects
    ADD COLUMN member_offset INTEGER NOT NULL DEFAULT 0;
