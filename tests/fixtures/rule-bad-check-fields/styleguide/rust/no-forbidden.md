---
type: Styleguide Rule
description: Never write the forbidden token.
language: rust
category: hygiene
priority: critical
pattern: "(unclosed"
---
# No Forbidden Token

The token must not appear.

## Bad

forbidden();
