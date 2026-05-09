use super::super::fanout::CommittedEventEnvelope;
use super::{
    AppendGuards, AppendReceipt, BatchAppendItem, Coordinate, Event, EventKind, Notification,
    StoreError, WriterState,
};
use crate::store::stats::HlcPoint;
use flume::Sender;

enum PendingFenceResponse {
    Single {
        respond: Sender<Result<AppendReceipt, StoreError>>,
        receipt: AppendReceipt,
    },
    Batch {
        respond: Sender<Result<Vec<AppendReceipt>, StoreError>>,
        receipts: Vec<AppendReceipt>,
    },
}

impl PendingFenceResponse {
    fn complete_cancelled(self) {
        match self {
            Self::Single { respond, .. } => {
                let _ = respond.send(Err(StoreError::VisibilityFenceCancelled));
            }
            Self::Batch { respond, .. } => {
                let _ = respond.send(Err(StoreError::VisibilityFenceCancelled));
            }
        }
    }

    fn complete_ok(self) {
        match self {
            Self::Single { respond, receipt } => {
                let _ = respond.send(Ok(receipt));
            }
            Self::Batch { respond, receipts } => {
                let _ = respond.send(Ok(receipts));
            }
        }
    }
}

pub(super) struct FenceLedger {
    pub(super) token: u64,
    pub(super) publish_up_to: Option<u64>,
    pub(super) frontier_point: Option<HlcPoint>,
    pub(super) notifications: Vec<Notification>,
    pub(super) envelopes: Vec<CommittedEventEnvelope>,
    responses: Vec<PendingFenceResponse>,
}

impl FenceLedger {
    pub(super) fn new(token: u64) -> Self {
        Self {
            token,
            publish_up_to: None,
            frontier_point: None,
            notifications: Vec::new(),
            envelopes: Vec::new(),
            responses: Vec::new(),
        }
    }

    pub(super) fn record_publish_up_to(&mut self, publish_up_to: u64, frontier_point: HlcPoint) {
        self.publish_up_to = Some(self.publish_up_to.unwrap_or(0).max(publish_up_to));
        self.frontier_point = Some(
            self.frontier_point
                .unwrap_or(HlcPoint::ORIGIN)
                .max(frontier_point),
        );
    }

    pub(super) fn extend_artifacts(
        &mut self,
        notifications: impl IntoIterator<Item = Notification>,
        envelopes: impl IntoIterator<Item = CommittedEventEnvelope>,
    ) {
        self.notifications.extend(notifications);
        self.envelopes.extend(envelopes);
    }

    fn push_response(&mut self, response: PendingFenceResponse) {
        self.responses.push(response);
    }

    fn complete_cancelled(self) {
        for response in self.responses {
            response.complete_cancelled();
        }
    }
}

#[derive(Debug)]
pub(super) enum DeferredReply {
    None,
    Sync {
        respond: Sender<Result<(), StoreError>>,
    },
    BeginVisibilityFence {
        token: u64,
        respond: Sender<Result<(), StoreError>>,
    },
    CommitVisibilityFence {
        token: u64,
        respond: Sender<Result<(), StoreError>>,
    },
    Shutdown {
        respond: Sender<Result<(), StoreError>>,
    },
}

impl DeferredReply {
    pub(super) fn send(
        self,
        state: &mut WriterState<'_>,
        sync_result: Result<(), StoreError>,
    ) -> Result<(), StoreError> {
        match self {
            Self::None => Ok(()),
            Self::Sync { respond } => {
                let _ = respond.send(sync_result);
                Ok(())
            }
            Self::BeginVisibilityFence { token, respond } => {
                let result = sync_result.and_then(|_| state.begin_visibility_fence(token));
                let _ = respond.send(result);
                Ok(())
            }
            Self::CommitVisibilityFence { token, respond } => {
                let result = sync_result.and_then(|_| state.commit_visibility_fence(token));
                let _ = respond.send(result);
                Ok(())
            }
            Self::Shutdown { respond } => {
                let _ = respond.send(sync_result);
                Ok(())
            }
        }
    }
}

#[derive(Debug)]
pub(super) struct CommandResult {
    pub(super) sync_event_delta: u32,
    pub(super) break_after_reply: bool,
    pub(super) must_sync_before_continue: bool,
    pub(super) exit_writer: bool,
    pub(super) deferred_reply: DeferredReply,
    pub(super) shutdown_drain_respond: Option<Sender<Result<(), StoreError>>>,
    pub(super) enter_group_commit_drain: bool,
}

impl CommandResult {
    pub(super) fn immediate(sync_event_delta: u32) -> Self {
        Self {
            sync_event_delta,
            break_after_reply: false,
            must_sync_before_continue: false,
            exit_writer: false,
            deferred_reply: DeferredReply::None,
            shutdown_drain_respond: None,
            enter_group_commit_drain: false,
        }
    }

    pub(super) fn break_after_reply(mut self) -> Self {
        self.break_after_reply = true;
        self
    }

    pub(super) fn break_after_reply_if(self, condition: bool) -> Self {
        if condition {
            self.break_after_reply()
        } else {
            self
        }
    }

    pub(super) fn with_sync(mut self, deferred_reply: DeferredReply) -> Self {
        self.must_sync_before_continue = true;
        self.deferred_reply = deferred_reply;
        self
    }

    pub(super) fn exit_writer(mut self) -> Self {
        self.exit_writer = true;
        self
    }

    pub(super) fn enter_shutdown_drain(mut self, respond: Sender<Result<(), StoreError>>) -> Self {
        self.exit_writer = true;
        self.shutdown_drain_respond = Some(respond);
        self
    }

    pub(super) fn enter_group_commit_drain(mut self) -> Self {
        self.enter_group_commit_drain = true;
        self
    }
}

impl WriterState<'_> {
    pub(super) fn auto_cancel_fence_on_shutdown(&mut self) {
        if let Some(fence) = self.fence_ledger.take() {
            tracing::warn!(
                token = fence.token,
                pending = fence.responses.len(),
                "auto-cancelling active visibility fence during shutdown"
            );
            let _ = self.index.cancel_visibility_fence(fence.token);
            if let Err(error) = self.persist_cancelled_visibility_ranges() {
                tracing::error!(
                    error = %error,
                    "failed to persist cancelled visibility ranges during shutdown"
                );
            }
            fence.complete_cancelled();
        }
    }

    fn with_matching_fence_ledger<R>(
        &mut self,
        token: u64,
        f: impl FnOnce(&mut Self, &mut FenceLedger) -> Result<R, StoreError>,
    ) -> Result<R, StoreError> {
        if self.fence_ledger.as_ref().map(|fence| fence.token) != Some(token) {
            return Err(StoreError::VisibilityFenceNotActive);
        }
        let Some(mut fence) = self.fence_ledger.take() else {
            return Err(StoreError::VisibilityFenceNotActive);
        };
        let result = f(self, &mut fence);
        self.fence_ledger = Some(fence);
        result
    }

    pub(super) fn handle_fence_append_command(
        &mut self,
        token: u64,
        coord: &Coordinate,
        event: Event<Vec<u8>>,
        kind: EventKind,
        guards: &AppendGuards,
        respond: Sender<Result<AppendReceipt, StoreError>>,
    ) -> Result<(), StoreError> {
        self.with_matching_fence_ledger(token, |state, fence| {
            let receipt = state.handle_append(coord, event, kind, guards, Some(fence))?;
            fence.push_response(PendingFenceResponse::Single { respond, receipt });
            Ok(())
        })
    }

    pub(super) fn handle_fence_append_batch_command(
        &mut self,
        token: u64,
        items: Vec<BatchAppendItem>,
        respond: Sender<Result<Vec<AppendReceipt>, StoreError>>,
    ) -> Result<(), StoreError> {
        self.with_matching_fence_ledger(token, |state, fence| {
            let receipts = state.handle_append_batch(items, Some(fence))?;
            fence.push_response(PendingFenceResponse::Batch { respond, receipts });
            Ok(())
        })
    }

    pub(super) fn begin_visibility_fence(&mut self, token: u64) -> Result<(), StoreError> {
        if self.fence_ledger.is_some() {
            return Err(StoreError::VisibilityFenceActive);
        }
        if self.index.active_visibility_fence() != Some(token) {
            return Err(StoreError::VisibilityFenceNotActive);
        }
        self.fence_ledger = Some(FenceLedger::new(token));
        Ok(())
    }

    pub(super) fn commit_visibility_fence(&mut self, token: u64) -> Result<(), StoreError> {
        let Some(fence) = self.fence_ledger.take() else {
            return Err(StoreError::VisibilityFenceNotActive);
        };
        if fence.token != token {
            self.fence_ledger = Some(fence);
            return Err(StoreError::VisibilityFenceNotActive);
        }

        let FenceLedger {
            publish_up_to,
            frontier_point,
            notifications,
            envelopes,
            responses,
            ..
        } = fence;
        self.fence_finish_then_broadcast(
            token,
            publish_up_to,
            frontier_point,
            notifications,
            envelopes,
        )?;
        for response in responses {
            response.complete_ok();
        }
        Ok(())
    }

    pub(super) fn cancel_visibility_fence(&mut self, token: u64) -> Result<(), StoreError> {
        let Some(fence) = self.fence_ledger.take() else {
            return Err(StoreError::VisibilityFenceNotActive);
        };
        if fence.token != token {
            self.fence_ledger = Some(fence);
            return Err(StoreError::VisibilityFenceNotActive);
        }

        self.index.cancel_visibility_fence(token)?;
        self.persist_cancelled_visibility_ranges()?;
        fence.complete_cancelled();
        Ok(())
    }

    fn persist_cancelled_visibility_ranges(&self) -> Result<(), StoreError> {
        crate::store::hidden_ranges::write_cancelled_ranges(
            &self.config.data_dir,
            &self.index.cancelled_visibility_ranges(),
        )
    }
}
