# Credential boundary

Refresh credentials belong to the interactive Windows user, never the system service. They are serialized only after user-scoped DPAPI encryption with account-bound optional entropy. Plaintext buffers are zeroized after use and all debug output redacts token material.

The service may request an `AccessCapability` from a separately hosted per-user broker. That capability contains only a short-lived access token and expiry. Tokens must travel over authenticated local IPC in later integration; they must never appear in command lines, environment variables, repository configuration, logs, or crash reports.

Machine-scoped DPAPI exists only as an explicit primitive and is forbidden for Drive OAuth records.
