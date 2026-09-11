# Requested wt capacity features

wt should provide a machine-wide capacity pool, task leases identifying active
output roots, configurable scratch reservations, and explanations mapping
retained artifacts to producing tasks. Admission and cleanup must use the same
lock and must never retire an active tree's output. The repository helper stays
small once those primitives exist.
