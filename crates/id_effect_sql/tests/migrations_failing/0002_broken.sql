-- The first statement succeeds; the second fails, so both roll back.
INSERT INTO gadgets VALUES (1);
INSERT INTO no_such_table VALUES (1);
