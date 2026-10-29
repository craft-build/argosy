---
type: Styleguide Rule
description: Never unwrap without a reason — every expect and assert carries a message with the relevant values.
language: rust
category: error-handling
---
# Expect a Reason

`unwrap()` is banned outside tests and `main`; use `expect` with a message
that restates *why* the invariant holds, including the values that broke
it.

## Good

```rust
let config = raw.expect("config is validated before workers start");
```

## Bad

```rust
let config = raw.unwrap();
```

A panic message that merely repeats the condition (`"config was None"`)
is no better than a bare unwrap.
