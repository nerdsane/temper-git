// Lineage Cedar policies.
//
// Lineage rows are metadata sidecars for forks and mutation summaries. Phase 5
// restricts writes to Apps.Fork / mutation-recording agents in commons mode.

permit(principal, action, resource is Lineage);
