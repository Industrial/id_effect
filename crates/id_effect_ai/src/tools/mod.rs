//! Typed tool calling (`feature = "tools"`): tools whose input and output have a `Schema`, the
//! `ToolSet` a call may use, and the deny-by-default rules of ADR 0015 D9 (only registered
//! tools run, arguments are decoded before a handler sees them, write tools need an explicit
//! policy, and every loop is bounded).
