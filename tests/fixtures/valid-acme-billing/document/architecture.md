---
type: Architecture Overview
description: How the ACME billing payment flow fits together.
tags:
  - architecture
---
# Payment Flow Architecture

The billing service receives usage events, aggregates them into invoices each
night, and settles them through the payment processor.
