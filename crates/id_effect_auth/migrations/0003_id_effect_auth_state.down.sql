-- Reverts 0003_id_effect_auth_state.up.sql (the index goes with its table).
DROP TABLE IF EXISTS id_effect_auth_failures;
DROP TABLE IF EXISTS id_effect_totp_steps;
