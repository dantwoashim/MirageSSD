# Native status policy

| Engine status | NTSTATUS |
| --- | --- |
| ok | `STATUS_SUCCESS` |
| invalid argument | `STATUS_INVALID_PARAMETER` |
| not found | `STATUS_OBJECT_NAME_NOT_FOUND` |
| access denied | `STATUS_ACCESS_DENIED` |
| would block | `STATUS_PENDING` |
| cancelled | `STATUS_CANCELLED` |
| integrity/backend/I/O failure | `STATUS_IO_DEVICE_ERROR` |
| unexpected internal failure | `STATUS_INTERNAL_ERROR` |

All mutation callbacks return `STATUS_MEDIA_WRITE_PROTECTED`. Directory/file misuse returns the corresponding filesystem status rather than a fabricated empty result.
