-- Group-commit barrier epoch. A transaction bumping `seq` under
-- synchronous=FULL fsyncs the WAL, which makes every earlier NORMAL commit
-- durable. The row carries no other meaning.
CREATE TABLE durability_barrier (
    id  INTEGER PRIMARY KEY CHECK (id = 1),
    seq INTEGER NOT NULL
) STRICT;

INSERT INTO durability_barrier VALUES (1, 0);
