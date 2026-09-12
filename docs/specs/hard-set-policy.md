# Hard-set policy

The mandatory startup set includes pages observed within the configured startup window in both the absolute minimum number and minimum fraction of representative sessions. Manual, severe-stall, and scan/map evidence can promote a page independently. Every output page records all reasons and its distinct source-session count; a one-off ordinary observation is never sufficient.

## Capsule coverage rule

Independently of the profile-derived set, the capsule always includes the first and last page of every file in the mount index. On-open probes (antivirus signature scans, content indexers, cache-manager sniffing) read file heads and tails through the game's own open handles; without this rule those reads miss the capsule and are recorded as seal violations even though no profile ever observed them.
