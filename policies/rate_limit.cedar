// RateLimit Cedar policies.
//
// Buckets are internal guardrail state. Operator mode remains permissive until
// commons middleware enforces token consumption in Phase 5.

permit(principal, action, resource is RateLimit);
