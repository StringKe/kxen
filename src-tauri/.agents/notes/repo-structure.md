---
note-type: convention
description: Repo structure
date: 2026-07-24
---

The root pnpm workspace contains only the `kxen-ui` desktop frontend package. The product website is an independent `website` pnpm package outside that workspace, and `src-tauri` is one Rust crate. Install and verification commands therefore run separately in the root and `website` directories.
