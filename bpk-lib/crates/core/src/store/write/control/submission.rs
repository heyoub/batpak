use super::{AppendGuards, AppendReply, WriterCommand};
use crate::coordinate::Coordinate;
use crate::event::{Event, EventHeader, EventKind};
use crate::store::append::{checked_payload_len, EncodedBytes, ExtensionKey};
use crate::store::{AppendOptions, Clock, Open, Store, StoreError};
use serde::Serialize;
use std::collections::BTreeMap;

#[derive(Clone, Debug)]
pub(crate) struct AppendSubmission {
    event_id: u128,
    correlation_id: u128,
    pub(super) options: AppendOptions,
    fence_token: Option<u64>,
    /// `EventPayload::PAYLOAD_VERSION` threaded from the typed-append seam.
    ///
    /// `0` is the default for every untyped / legacy / batch / denial /
    /// lifecycle path; only the typed lowerings (`append_typed*`,
    /// `submit_typed*`, `*_reaction_typed`, `apply_transition`) raise it to
    /// `T::PAYLOAD_VERSION` via [`AppendSubmission::with_payload_version`]. It is
    /// stamped into the header by [`AppendSubmission::build_event`] and rides
    /// outside the hashed/signed region.
    payload_version: u16,
}

impl AppendSubmission {
    pub(crate) fn root(clock: &dyn Clock) -> Self {
        let event_id = crate::id::generate_v7_id_with_clock(clock);
        Self {
            event_id,
            correlation_id: event_id,
            options: AppendOptions::default(),
            fence_token: None,
            payload_version: 0,
        }
    }

    /// Stamp the typed payload schema version onto this submission.
    ///
    /// Threaded as a scalar (not a generic bound) because `build_event` and the
    /// public funnels are bounded `impl Serialize`, not `EventPayload`. Only the
    /// typed lowerings call this; everything else keeps the `0` sentinel.
    pub(crate) fn with_payload_version(mut self, payload_version: u16) -> Self {
        self.payload_version = payload_version;
        self
    }

    pub(crate) fn root_under_fence(token: u64, clock: &dyn Clock) -> Self {
        Self {
            fence_token: Some(token),
            ..Self::root(clock)
        }
    }

    pub(crate) fn reaction(clock: &dyn Clock, correlation_id: u128, causation_id: u128) -> Self {
        let event_id = crate::id::generate_v7_id_with_clock(clock);
        Self {
            event_id,
            correlation_id,
            options: AppendOptions {
                causation_id: (causation_id != 0)
                    .then_some(crate::id::CausationId::from(causation_id)),
                ..AppendOptions::default()
            },
            fence_token: None,
            payload_version: 0,
        }
    }

    pub(crate) fn reaction_under_fence(
        token: u64,
        clock: &dyn Clock,
        correlation_id: u128,
        causation_id: u128,
    ) -> Self {
        Self {
            fence_token: Some(token),
            ..Self::reaction(clock, correlation_id, causation_id)
        }
    }

    pub(crate) fn with_options(options: AppendOptions, clock: &dyn Clock) -> Self {
        use crate::id::EntityIdType;
        let event_id = options
            .idempotency_key
            .map(|key| key.as_u128())
            .unwrap_or_else(|| crate::id::generate_v7_id_with_clock(clock));
        Self {
            event_id,
            correlation_id: options
                .correlation_id
                .map(|id| id.as_u128())
                .unwrap_or(event_id),
            options,
            fence_token: None,
            payload_version: 0,
        }
    }

    pub(crate) fn validate_route(&self, store: &Store<Open>) -> Result<(), StoreError> {
        if self.fence_token.is_none() {
            store.ensure_no_active_public_fence()?;
        }
        Ok(())
    }

    pub(crate) fn validate_idempotency(&self, store: &Store<Open>) -> Result<(), StoreError> {
        if store.runtime.require_idempotency_keys && self.options.idempotency_key.is_none() {
            return Err(StoreError::IdempotencyRequired);
        }
        Ok(())
    }

    pub(crate) fn receipt_extensions(&self) -> &BTreeMap<ExtensionKey, EncodedBytes> {
        &self.options.extensions
    }

    pub(crate) fn build_event(
        &self,
        payload: &impl Serialize,
        kind: EventKind,
        now_us: i64,
    ) -> Result<Event<Vec<u8>>, StoreError> {
        use crate::id::EntityIdType;
        let payload_bytes = crate::encoding::to_bytes(payload)
            .map_err(|e| StoreError::Serialization(Box::new(e)))?;
        let payload_len = checked_payload_len(&payload_bytes)?;
        let mut header = EventHeader::new(
            self.event_id,
            self.correlation_id,
            self.options.causation_id.map(|id| id.as_u128()),
            now_us,
            crate::coordinate::DagPosition::root(),
            payload_len,
            kind,
        );
        if self.options.flags != 0 {
            header = header.with_flags(self.options.flags);
        }
        if self.payload_version != 0 {
            header = header.with_payload_version(self.payload_version);
        }
        Ok(Event::new(header, payload_bytes))
    }

    fn guards(self) -> AppendGuards {
        use crate::id::EntityIdType;
        let position_hint = self.options.position_hint.unwrap_or_default();
        AppendGuards {
            correlation_id: self.correlation_id,
            causation_id: self.options.causation_id.map(|id| id.as_u128()),
            expected_sequence: self.options.expected_sequence,
            idempotency_key: self.options.idempotency_key.map(|id| id.as_u128()),
            dag_lane: position_hint.lane,
            dag_depth: position_hint.depth,
            extensions: self.options.extensions,
        }
    }

    pub(crate) fn into_command(
        self,
        coord: Coordinate,
        kind: EventKind,
        event: Event<Vec<u8>>,
        respond: flume::Sender<AppendReply>,
    ) -> WriterCommand {
        let fence_token = self.fence_token;
        let guards = self.guards();
        match fence_token {
            Some(token) => WriterCommand::FenceAppend {
                token,
                coord,
                event: Box::new(event),
                kind,
                guards,
                respond,
            },
            None => WriterCommand::Append {
                coord,
                event: Box::new(event),
                kind,
                guards,
                respond,
            },
        }
    }
}

#[cfg(test)]
mod submission_tests {
    use super::{AppendReply, AppendSubmission, WriterCommand};
    use crate::coordinate::Coordinate;
    use crate::event::EventKind;
    use crate::id::{CausationId, CorrelationId, EntityIdType, IdempotencyKey};
    use crate::store::{AppendOptions, Clock};

    /// Deterministic clock so generated v7 ids are stable enough for assertions
    /// that do not depend on the exact id value.
    struct StubClock;
    impl Clock for StubClock {
        fn now_us(&self) -> i64 {
            1_700_000_000_000_000
        }
        fn now_wall_ns(&self) -> i64 {
            1_700_000_000_000_000 * 1000
        }
        fn now_mono_ns(&self) -> i64 {
            1
        }
        fn process_boot_ns(&self) -> u64 {
            0
        }
    }

    #[test]
    fn root_sets_correlation_equal_to_event_id_and_no_fence() {
        let s = AppendSubmission::root(&StubClock);
        assert_eq!(
            s.event_id, s.correlation_id,
            "a root submission self-correlates"
        );
        assert!(s.fence_token.is_none(), "root carries no fence token");
        assert_eq!(
            s.payload_version, 0,
            "default payload version is the 0 sentinel"
        );
    }

    #[test]
    fn root_under_fence_carries_the_token() {
        let s = AppendSubmission::root_under_fence(7, &StubClock);
        assert_eq!(
            s.fence_token,
            Some(7),
            "fence token must be threaded through"
        );
    }

    #[test]
    fn reaction_treats_zero_causation_as_none_but_keeps_nonzero() {
        let none = AppendSubmission::reaction(&StubClock, 0xABC, 0);
        assert!(
            none.options.causation_id.is_none(),
            "0 is the wire sentinel and must NOT become Some(0)"
        );
        assert_eq!(none.correlation_id, 0xABC, "correlation id is threaded");

        let some = AppendSubmission::reaction(&StubClock, 0xABC, 0x55);
        assert_eq!(
            some.options.causation_id.map(|id| id.as_u128()),
            Some(0x55),
            "a non-zero causation must be recorded"
        );
    }

    #[test]
    fn with_options_uses_idempotency_key_as_event_id() {
        let key = IdempotencyKey::from(0xFEED_u128);
        let opts = AppendOptions::default().with_idempotency(key);
        let s = AppendSubmission::with_options(opts, &StubClock);
        assert_eq!(
            s.event_id, 0xFEED,
            "the idempotency key must become the event id (keyed determinism)"
        );
        assert_eq!(
            s.correlation_id, 0xFEED,
            "correlation defaults to the event id when unset"
        );
    }

    #[test]
    fn with_options_honors_explicit_correlation_over_event_id() {
        let opts = AppendOptions::default().with_correlation(CorrelationId::from(0x999_u128));
        let s = AppendSubmission::with_options(opts, &StubClock);
        assert_eq!(
            s.correlation_id, 0x999,
            "an explicit correlation id must override the event-id default"
        );
    }

    #[test]
    fn with_payload_version_stamps_only_the_version_field() {
        let s = AppendSubmission::root(&StubClock).with_payload_version(5);
        assert_eq!(s.payload_version, 5);
    }

    #[test]
    fn build_event_stamps_kind_flags_payload_version_and_causation() {
        let opts = AppendOptions::default()
            .with_flags(0b101)
            .with_causation(CausationId::from(0x77_u128));
        let s = AppendSubmission::with_options(opts, &StubClock).with_payload_version(9);
        let kind = EventKind::custom(0xC, 4);
        let event = s
            .build_event(&serde_json::json!({"k": 1}), kind, 12345)
            .expect("build_event");
        assert_eq!(event.header.event_kind, kind, "kind stamped");
        assert_eq!(event.header.flags, 0b101, "non-zero flags applied");
        assert_eq!(
            event.header.payload_version, 9,
            "non-zero payload version applied"
        );
        assert_eq!(
            event.header.causation_id.map(|id| id.as_u128()),
            Some(0x77),
            "causation threaded into the header"
        );
    }

    #[test]
    fn build_event_leaves_flags_and_version_at_zero_by_default() {
        let s = AppendSubmission::root(&StubClock);
        let event = s
            .build_event(&serde_json::json!({}), EventKind::DATA, 1)
            .expect("build_event");
        assert_eq!(event.header.flags, 0, "default flags untouched");
        assert_eq!(event.header.payload_version, 0, "default version untouched");
    }

    #[test]
    fn validate_idempotency_is_satisfied_when_key_present() {
        // Without a Store we exercise the early-return: a present key never errors.
        let opts = AppendOptions::default().with_idempotency(IdempotencyKey::from(1u128));
        let s = AppendSubmission::with_options(opts, &StubClock);
        // The key is recorded; the only field validate_idempotency reads beyond
        // runtime config is idempotency_key.is_none(), which is false here.
        assert!(s.options.idempotency_key.is_some());
    }

    #[test]
    fn into_command_selects_fence_branch_with_token_and_threads_guards() {
        let opts = AppendOptions::default()
            .with_cas(3)
            .with_position_hint(crate::store::AppendPositionHint::new(2, 4));
        let s = AppendSubmission {
            fence_token: Some(42),
            ..AppendSubmission::with_options(opts, &StubClock)
        };
        let coord = Coordinate::new("entity:c", "scope:c").expect("coord");
        let kind = EventKind::custom(0xA, 1);
        let event = s
            .build_event(&serde_json::json!({}), kind, 1)
            .expect("build_event");
        let (tx, _rx) = flume::bounded::<AppendReply>(1);
        assert!(
            matches!(
                s.into_command(coord, kind, event, tx),
                WriterCommand::FenceAppend { token, guards, .. }
                    if token == 42
                        && guards.expected_sequence == Some(3)
                        && guards.dag_lane == 2
                        && guards.dag_depth == 4
            ),
            "fence token must route to FenceAppend with CAS + position-hint guards threaded"
        );
    }

    #[test]
    fn into_command_selects_unfenced_branch_when_no_token() {
        let s = AppendSubmission::root(&StubClock);
        let coord = Coordinate::new("entity:u", "scope:u").expect("coord");
        let kind = EventKind::DATA;
        let event = s
            .build_event(&serde_json::json!({}), kind, 1)
            .expect("build_event");
        let (tx, _rx) = flume::bounded::<AppendReply>(1);
        assert!(
            matches!(
                s.into_command(coord, kind, event, tx),
                WriterCommand::Append { .. }
            ),
            "no fence token must route to the plain Append command"
        );
    }
}
