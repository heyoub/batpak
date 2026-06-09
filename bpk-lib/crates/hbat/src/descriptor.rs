//! Declarative macro for submitting [`crate::manifest::EventDescriptorRegistration`]
//! entries without repeating inventory boilerplate.

/// Register one `EventPayload`-deriving type with the manifest inventory.
///
/// Expands to `inventory::submit! { EventDescriptorRegistration { ... } }`.
/// Field `order` values are assigned by a recursive internal helper (no
/// repetition index). Fixtures use UFCS on [`crate::EventPayloadFixture`].
#[macro_export]
macro_rules! hbat_event_descriptor {
    (
        type = $ty:ty,
        schema_ref = $schema_ref:expr,
        ts_name = $ts_name:literal,
        fields = [ $( ($wire:literal, $token:literal) ),* $(,)? ] $(,)?
    ) => {
        inventory::submit! {
            $crate::manifest::EventDescriptorRegistration {
                rust_type: concat!(module_path!(), "::", stringify!($ty)),
                ts_name: $ts_name,
                schema_ref: $schema_ref,
                kind_bits: <$ty>::KIND.as_raw_u16(),
                fields: $crate::hbat_event_descriptor!(
                    @fields_inner
                    0usize,
                    [ $( ($wire, $token) ),* ],
                    ACC: []
                ),
                fixture_bytes: || {
                    batpak::encoding::to_bytes(
                        &<$ty as $crate::EventPayloadFixture>::fixture_value(),
                    )
                    .ok()
                },
                fixture_json: || {
                    serde_json::to_value(
                        <$ty as $crate::EventPayloadFixture>::fixture_value(),
                    )
                    .ok()
                },
            }
        }
    };

    (@fields_inner $idx:expr, [ ], ACC: [ $( $acc:expr ),* $(,)? ]) => {
        &[ $( $acc ),* ] as &[$crate::manifest::FieldRow]
    };

    (
        @fields_inner
        $idx:expr,
        [ ($wire:literal, $token:literal) $(, ($rest_wire:literal, $rest_token:literal) )* $(,)? ],
        ACC: [ $( $acc:expr ),* $(,)? ]
    ) => {
        $crate::hbat_event_descriptor!(
            @fields_inner
            ($idx + 1usize),
            [ $( ($rest_wire, $rest_token) ),* ],
            ACC: [
                $( $acc, )*
                $crate::manifest::FieldRow {
                    wire_name: $wire,
                    type_token: $token,
                    order: $idx,
                }
            ]
        )
    };
}

#[cfg(test)]
mod tests {
    use crate::manifest::descriptors;

    #[test]
    fn macro_registered_events_still_number_twenty_one() {
        let snap = descriptors().expect("build manifest snapshot");
        assert_eq!(snap.events.len(), 21);
        assert_eq!(snap.operations.len(), 10);
    }
}
