-- Reverts 0001_audit_log.up.sql (the audit records are lost).
DROP TABLE IF EXISTS audit_log_head;
DROP TABLE IF EXISTS audit_log;
