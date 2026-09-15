-- Preserve exact scope identity even when the database default collation folds
-- case or accents. All existing values remain unchanged; only comparisons and
-- indexes become binary.
ALTER TABLE aj_document
    MODIFY scope VARCHAR(64) CHARACTER SET utf8mb4 COLLATE utf8mb4_bin NOT NULL;

ALTER TABLE aj_document_revision
    MODIFY scope VARCHAR(64) CHARACTER SET utf8mb4 COLLATE utf8mb4_bin NOT NULL;

ALTER TABLE srv_session
    MODIFY scope VARCHAR(64) CHARACTER SET utf8mb4 COLLATE utf8mb4_bin NOT NULL;
