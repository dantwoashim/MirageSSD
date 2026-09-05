# Google Drive operational policy

- Scope: `drive.file`, so MirageSSD can access files it creates or the user explicitly opens with the app without broad Drive visibility.
- Reads address immutable provider file IDs and require HTTP 206 plus the exact `Content-Range` and body length. A whole-body HTTP 200 response is rejected.
- 401 requires user-token refresh; permission 403 is permanent for the operation; quota/rate 403 and 429 pause speculative traffic and honor a bounded numeric `Retry-After`; 408 and 5xx are transient; 404 is missing-object evidence.
- Response bodies and authorization headers are never included in public errors.
- Quota-unit estimates are updateable policy inputs, not correctness assumptions.
- Interactive OAuth uses loopback redirect, a cryptographically random state, and PKCE S256.
