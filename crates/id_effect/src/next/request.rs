//! Request batching and caching: `Effect::request` over a typed resolver. Empty until
//! `p-request-batch` fills it (requests issued together in one `all` are deduplicated and sent
//! as one resolver call) and `p-request-cache` adds caching scoped to a `Layer` or `Scope`.
