---
note-type: convention
description: All LLM traffic must go through MRM
date: 2026-07-24
---

ModelResourceManager is the single choke point for text generation, embedding, Provider-native search, cloud audio transcription, role routing, account selection, concurrency, RPM, token and cost budgets, circuit breaking, and degradation chains. Each such Provider call must use `begin_call()` or `begin_probe_call()` and call `start()` on the returned `CallPermit` immediately before network I/O. The resulting RAII `Slot` records usage and releases admission capacity on drop. Every billable Provider path must also create a durable Provider attempt before crossing the network boundary, then settle observed usage or explicit UNKNOWN with the same operation ID; probes that do not perform inference remain separate. Voice additionally requires explicit cloud fallback consent for Apple audio upload and settles sent audio as UNKNOWN because the token ledger cannot express duration billing.
