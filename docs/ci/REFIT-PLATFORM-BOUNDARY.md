# Disk refit platform boundary

**Status:** ready to build · **Written:** 2026-11-19

## 1. Problem

`state::disk_refit_ready` names `vodencode::dv_runtime::ToolSet` on every platform, while dv_runtime correctly compiles only on Linux. The inherited daemon therefore cannot compile on macOS. Do not enable the Linux native execution module on unsupported platforms.

## 2. Contract and ownership

Own private `vodencode::disk_refit_tools` in a new `disk_refit_tools.rs`, its module declaration in vodencode.rs, and only the two qualified ToolSet paths in state.rs. Linux reexports the exact original ToolSet, preserving capture, scanner path, source custody, helper identity and final currentness validation. Other platforms expose an uninhabited capability: capture returns explicit unsupported-FEL refusal; no scanner, identity or currentness value can be manufactured. The existing complete MEL classification returns None before tool capture, preserving ordinary MEL conversion behavior.

## 3. Validation

Establish the inherited two-error baseline (already observed against exact09d). Compile macOS daemon all targets after repair and run a non-Linux capture-refusal regression. Linux type identity is checked by an actual coercion between the two Arc paths; dv_runtime itself has no diff. Exercise the existing disk classification helper for MEL where the fixture permits it. No suppression or assertion weakening. Record actual checks and remaining host qualification separately.
