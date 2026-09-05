# Security policy

MirageSSD is an engineering preview, without a production support guarantee or independently certified release.

Do not publish tokens, OAuth configuration, private file listings, recovery keys, or unredacted logs in issues.

Use GitHub's private vulnerability reporting option if available. Otherwise contact the maintainer through their GitHub profile and ask for a private channel before sharing sensitive details.

Include the affected revision, Windows version, a disposable-data reproduction, and expected versus observed behavior.

## Priority failures

Wrong contents, lost pending writes, path escape, cross-user access, authentication bypass, credential exposure, and unsafe source deletion are blocking storage-integrity defects.

## Boundaries

The writable drive accepts data into a local write-back cache before remote upload completes. Ordinary file contents are not encrypted by MirageSSD before upload.

Credentials use per-user Windows DPAPI and restricted filesystem permissions. These do not protect against a compromised Windows account or administrator. Cached contents and attribute journals remain local.

Separate encrypted repositories and backup helpers have their own recovery requirements. Their guarantees do not apply automatically to the writable drive.
