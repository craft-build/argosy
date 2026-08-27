---
type: Reference
description: The payment processor rate-limits retries in a subtle way.
---
# Processor Rate-Limit Behavior

Promoted from `memory/gotchas.md`: the processor counts a retry against the
rate limit of the *original* request timestamp, not the retry time.
