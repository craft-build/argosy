---
type: Decision Record
description: Cache computed invoice totals for five minutes.
tags:
  - decision
---
# Decision: Short-lived invoice-total cache

Invoice totals are expensive to recompute for large accounts. We cache them
for five minutes, accepting brief staleness during high-velocity billing runs.
