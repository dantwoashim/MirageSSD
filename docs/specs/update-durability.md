# Update durability

Writes are accepted only through a generation/update-bound context. MirageSSD constructs every affected logical page, then persists the entire mutation batch and resulting file size atomically before exposing it in the overlay or acknowledging the write. Partial first writes fetch the base page; aligned full-page overwrites do not. Reads choose the durable overlay page first and otherwise use the immutable base generation.
