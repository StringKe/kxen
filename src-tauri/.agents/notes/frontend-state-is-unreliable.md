---
note-type: pitfall
description: Frontend state is unreliable
date: 2026-07-24
---

Backend durable stores and runtime registries are authoritative. AppState locks own ephemeral coordination such as active run tokens, Approval waiters, event subscriptions, and in-process task handles. Never use UI state for control decisions or resumption; reconcile snapshots with live events by stable identity and invalidate stale request generations.
