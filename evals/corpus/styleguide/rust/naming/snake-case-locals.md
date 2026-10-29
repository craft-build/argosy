---
type: Styleguide Rule
description: Local variables use snake_case; acronyms in type names are one word, UpperCamelCase.
language: rust
category: naming
---
# Snake-case Locals

Locals and functions use `snake_case`; types and traits use
`UpperCamelCase` with acronyms folded into one word (`Uuid`, not `UUID`).

## Good

```rust
let retry_count = 0;
struct SessionId(String);
```

## Bad

```rust
let retryCount = 0;
struct SessionID(String);
```

Conversion prefixes keep their order: `as_mut_slice`, never
`as_slice_mut`.
