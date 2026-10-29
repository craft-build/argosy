---
type: Styleguide Rule
description: Error display messages are lowercase, concise, and end without punctuation.
language: rust
category: error-handling
---
# Lowercase Error Messages

Error `Display` messages read as sentence fragments: lowercase first
letter, no trailing punctuation, values interpolated inline.

## Good

```rust
#[snafu(display("index was built with a different embedding model: {reason}"))]
```

## Bad

```rust
#[snafu(display("Index was built with a different embedding model!"))]
```

Uppercase starts and exclamation marks belong in user-facing alerts, not
in error types.
