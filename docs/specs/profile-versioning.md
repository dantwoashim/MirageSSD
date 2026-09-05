# Profile version transfer

Exact page-hash identity transfers with full confidence even if logical placement changes. A policy may transfer only cluster role across the same stable file/offset/length at explicitly reduced confidence; this never asserts that the old content hash exists. Removed logical pages are invalidated, and rename/repack transfer requires separate identity evidence.
