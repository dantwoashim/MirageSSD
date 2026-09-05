# ETW capture boundary

MirageSSD owns a uniquely named, real-time Windows kernel session configured only for file-I/O and file-name initialization events. The RAII handle stops the session on every normal return and during unwinding. Buffers are bounded and kernel loss counters are retained.

Raw events are correlated in bounded maps, filtered to the selected canonical game root, and written as independently checksummed atomic segments. Exports contain relative in-root paths only; unknown and outside-root activity is counted rather than disclosed.
