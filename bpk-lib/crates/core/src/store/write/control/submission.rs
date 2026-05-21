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
}

impl AppendSubmission {
    pub(crate) fn root(clock: &dyn Clock) -> Self {
        let event_id = crate::id::generate_v7_id_with_clock(clock);
        Self {
            event_id,
            correlation_id: event_id,
            options: AppendOptions::default(),
            fence_token: None,
        }
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
