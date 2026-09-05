# Native update boundary

Native executable, library, configuration, and mutable files remain outside the virtual overlay. Before an updater runs, selected relative paths are canonicalized under the registered game root and copied into an external rollback tree with length, BLAKE3 hash, and read-only metadata. Restore verifies snapshot bytes before atomic replacement and processes records in reverse order. Traversal, symlink/reparse escape, and rollback-inside-game-root are rejected.
