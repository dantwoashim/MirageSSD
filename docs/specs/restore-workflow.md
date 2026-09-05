# Fresh restore workflow

Fresh restore starts from an empty state root and a user-confirmed repository trust verifier. It enumerates and validates the signed commit chain, downloads only commit and manifest metadata, validates every referenced pack by provider stat, compiles an immutable mount index, creates an empty sparse cache arena, and reports every native executable/library/configuration/mutable path required from a legitimate local installation.

It never synthesizes native shell files and never downloads whole packs. Mounting remains forbidden until a compatible local shell is supplied and separately validated.
