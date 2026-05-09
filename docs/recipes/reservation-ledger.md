# Reservation Ledger

Agent surface task: `reservation_ledger`.

Problem: simulate abstract reservations and reconcile open/terminal buckets.

Correct API: `ReservationTransition`, `simulate_reservation_ledger`,
`reservation_reconciliation_report`.

Minimal code is mirrored by `templates/reservation-ledger`.

Wrong tempting move: encode business inventory, billing, or scheduling meanings
inside reservation primitives.

Test command: `cargo test -p batpak --test lane_b4_reservation_substrate --all-features`.

Invariant protected: reservation simulation is deterministic on normalized
transition order.
